// Command openflux-engine is the minimal Go shim that reuses the OpenFlux Go core
// (yandex transport + tunnel + gateway/SOCKS5) as a child process managed by the Rust
// CLI. Kept deliberately thin: the Rust side owns configuration, process lifecycle and
// OS routing; this binary runs the tunnel engine in one of these modes:
//
//   - SOCKS5 (default): a local SOCKS5 listener, for system-proxy mode.
//   - TUN (--tun-fd on Unix / --tun-read+--tun-write on Windows): the gvisor gateway
//     reading/writing a TUN device handed over by the CLI, for full-system capture.
//     Mirrors what mobile.StartTunnel does on Android.
//   - exit (--exit): run as the far end of the tunnel (an exit node) for other peers,
//     dialing the real destinations on this machine's behalf. No TUN fd or listener
//     needed; the tunnel carries the peers' traffic.
//
// Core features the shim can turn on: multiple parallel WebSocket streams
// (--streams, multistream), per-domain split tunneling for TUN mode
// (--split-mode/--split-sites, exclusive or inclusive), and encrypted DNS upstreams
// (--dns accepts plain, tls://… for DoT and https://… for DoH).
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"universal-bypass-tool/gateway"
	"universal-bypass-tool/socks5"
	"universal-bypass-tool/transport"
	"universal-bypass-tool/transport/yandex"
	"universal-bypass-tool/tunnel"
	"universal-bypass-tool/utils"
)

func main() {
	url := flag.String("url", "", "Yandex Docs document URL")
	socksAddr := flag.String("socks5", "127.0.0.1:1080", "SOCKS5 listen address (SOCKS5 mode)")
	tunFD := flag.Int("tun-fd", 0, "inherited TUN fd (Unix); when > 0 runs the gateway in TUN mode")
	tunRead := flag.Int("tun-read", 0, "inherited TUN read handle (Windows); with --tun-write runs the gateway in TUN mode")
	tunWrite := flag.Int("tun-write", 0, "inherited TUN write handle (Windows); with --tun-read runs the gateway in TUN mode")
	exit := flag.Bool("exit", false, "run as an exit node for other peers instead of a local client")
	exitMode := flag.String("exit-mode", "raw", "exit-node mode: raw (needs root, full TCP/UDP) or proxy (no root, TCP-only)")
	streams := flag.Int("streams", 1, "number of parallel WebSocket streams (multistream)")
	splitMode := flag.String("split-mode", "", "TUN split tunneling: 'exclude' (listed sites bypass the tunnel) or 'include' (only listed sites use the tunnel)")
	splitSites := flag.String("split-sites", "", "comma-separated domains/IPs for --split-mode (suffix wildcards like '*.ru' allowed)")
	dns := flag.String("dns", "77.88.8.8", "upstream DNS for the TUN gateway (plain IP[:port], tls://host for DoT, https://host/path for DoH)")
	sockMark := flag.Int("sock-mark", 0, "SO_MARK set on the transport's own sockets (lets TUN-mode routing exempt them from its own tunnel)")
	token := flag.String("token", "", "e2e key token; enables encrypted self-compression for this key")
	mtu := flag.Uint("mtu", 0, "optional tunnel MTU override")
	debug := flag.Bool("debug", false, "verbose debug logging")
	flag.Parse()

	if *url == "" {
		log.Fatalf("[ENGINE] -url is required")
	}
	if *streams < 1 {
		log.Fatalf("[ENGINE] -streams must be >= 1")
	}
	if !*exit && *tunFD <= 0 && *tunRead <= 0 && *socksAddr == "" {
		log.Fatalf("[ENGINE] either -tun-fd/--tun-read, -socks5, or -exit is required")
	}

	if *debug {
		utils.EnableDebug()
	}
	if *sockMark != 0 {
		// protectControl only runs when a protector is installed. SO_MARK needs
		// CAP_NET_ADMIN; without it the setsockopt fails and the socket is still usable, so
		// always return true (never treat a failed mark as a dial failure). See
		// sockmark_unix.go / sockmark_windows.go.
		installSockMarkProtector(*sockMark)
	}
	if *tunFD > 0 || *tunRead > 0 {
		// The transport marks its WS/HTTP sockets, but name resolution would otherwise use
		// the OS resolver (unmarked) and get captured by our own TUN before the tunnel is
		// up - a bootstrap deadlock. Route the engine's own lookups through the marked
		// resolver so they always leave via the physical link.
		net.DefaultResolver = transport.ProtectedResolver()
	}

	// One transport per stream; >1 turns on multistream (each stream a parallel WS
	// connection, frames hashed to a stream by port-pair).
	var trans transport.Transport
	if *streams <= 1 {
		tr := buildStream(*url, *token, *exit, 0)
		if err := tr.Start(); err != nil {
			log.Fatalf("[ENGINE] start transport: %v", err)
		}
		trans = tr
	} else {
		list := make([]transport.Transport, *streams)
		for i := range list {
			tr := buildStream(*url, *token, *exit, i)
			if err := tr.Start(); err != nil {
				log.Fatalf("[ENGINE] start transport stream %d: %v", i, err)
			}
			list[i] = tr
		}
		ms := transport.NewMultiStreamTransport(list)
		ms.SetEventCallback(func(code, detail string) {
			log.Printf("[TUNNEL] event %s (attempt %s)", code, detail)
		})
		log.Printf("[ENGINE] multistream: %d streams", *streams)
		trans = ms
	}

	em, err := tunnel.ParseExitMode(*exitMode)
	if err != nil {
		log.Fatalf("[ENGINE] bad -exit-mode %q: %v", *exitMode, err)
	}
	tun := tunnel.NewTCPTunnelMode(trans, *exit, em)
	if *mtu > 0 {
		tun.SetMTU(uint32(*mtu))
	}
	defer tun.Close()
	defer trans.Stop()

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	switch {
	case *exit:
		runExitMode(ctx, tun, trans)
	case *tunFD > 0 || (*tunRead > 0 && *tunWrite > 0):
		r, w, cleanup := tunFiles(*tunFD, *tunRead, *tunWrite)
		defer cleanup()
		runTunMode(ctx, r, w, *dns, *splitMode, *splitSites, tun, trans)
	default:
		runSocksMode(ctx, *socksAddr, tun, trans)
	}

	log.Printf("stopping")
}

// buildStream creates a single yandex transport, wired like the app's wrapYandex: batch
// compression (or e2e-encrypted self-compression when a key token is present). Multistream
// runs give each stream its own e2e key material via EnableEncryptedSelfCompressionForStream.
func buildStream(url, token string, isExitNode bool, streamIndex int) *yandex.YandexDocsTransport {
	t := yandex.NewYandexDocsTransport(url, transport.DefaultConfig())
	t.SetEventCallback(func(code, detail string) {
		label := ""
		if streamIndex > 0 {
			label = fmt.Sprintf(" (stream %d)", streamIndex)
		}
		log.Printf("[TUNNEL] event %s%s (attempt %s)", code, label, detail)
	})
	if token != "" {
		if streamIndex > 0 {
			t.EnableEncryptedSelfCompressionForStream(token, isExitNode, streamIndex)
		} else {
			t.EnableEncryptedSelfCompression(token, isExitNode)
		}
	} else {
		t.EnableSelfCompression()
	}
	return t
}

// startWatchdog logs, unconditionally (no --debug needed), whether the tunnel is actually
// carrying traffic, keyed off the stack's received-packet counter. A mode that came up but
// never connects (or connects then churns) is the classic cause of "TUN is up and the
// internet is dead" - this makes it visible in engine.log instead of silent.
func startWatchdog(ctx context.Context, mode string, counter func() uint64, connected interface{ IsConnected() bool }) {
	go func() {
		ticker := time.NewTicker(5 * time.Second)
		defer ticker.Stop()
		var last uint64
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				cur := counter()
				diff := cur - last
				last = cur
				switch {
				case diff > 0:
					log.Printf("[TRAFFIC] %s active: %d pkts/5s (total %d)", mode, diff, cur)
				case !connected.IsConnected():
					log.Printf("[TRAFFIC] %s idle and transport disconnected - traffic is being dropped", mode)
				}
			}
		}
	}()
}

// packetsRecv is the watchdog's received-packet counter: the gateway/tunnel no longer
// expose their own counter, but the transport stack does (and for multistream it is
// already the aggregate across streams).
func packetsRecv(trans transport.Transport) func() uint64 {
	return func() uint64 { return trans.Stats().PacketsRecv }
}

// runTunMode wires the gvisor gateway to the TUN device the CLI created, giving full-system
// TCP/UDP/DNS through the tunnel (the same role mobile.StartTunnel plays on Android).
// Split-tunnel and DNS-policy apply here: the gateway decides per destination whether to
// relay through the tunnel or bypass it (direct, on the marked physical-link sockets). DoT
// (tls://) and DoH (https://) --dns values are honored by gateway's upstream parser.
func runTunMode(ctx context.Context, r io.Reader, w io.Writer, dns, splitMode, splitSites string, tun *tunnel.TCPTunnel, trans transport.Transport) {
	var policy *gateway.SitePolicy
	if splitMode != "" {
		policy = gateway.NewSitePolicy(gateway.ParseSiteSplitMode(splitMode), splitDomains(splitSites))
		log.Printf("[TUNNEL] split mode %s: %s", splitMode, strings.Join(splitDomains(splitSites), ", "))
	}

	gw := gateway.NewServerWithPolicy(tun, dns, policy)
	if err := gw.Start(r, w); err != nil {
		log.Fatalf("[ENGINE] gateway start: %v", err)
	}
	defer gw.Close()

	log.Printf("ready tun dns=%s", dns)
	// from here on, the gateway speaks for the interface; the watchdog keeps engine.log
	// honest about whether packets are actually flowing.
	startWatchdog(ctx, "tun", packetsRecv(trans), trans)
	<-ctx.Done()
}

// runSocksMode exposes the tunnel as a local SOCKS5 listener for system-proxy mode.
func runSocksMode(ctx context.Context, addr string, tun *tunnel.TCPTunnel, trans transport.Transport) {
	// Pre-bind to surface an address conflict before declaring ourselves ready, then let
	// the socks5 package own the listener from here on.
	if probe, err := net.Listen("tcp", addr); err != nil {
		log.Fatalf("[ENGINE] socks5 listen %s: %v", addr, err)
	} else {
		_ = probe.Close()
	}
	server := socks5.NewSOCKS5Server(addr, tun)

	log.Printf("ready socks5=%s", addr)

	serverErr := make(chan error, 1)
	go func() { serverErr <- server.Start() }()
	startWatchdog(ctx, "socks", packetsRecv(trans), trans)

	select {
	case err := <-serverErr:
		if err != nil {
			log.Fatalf("[ENGINE] socks5 server: %v", err)
		}
	case <-ctx.Done():
	}

	_ = server.Stop()
}

// runExitMode keeps the tunnel engine alive as the far end for other peers: inbound
// flows arrive over the transport, are handled by the gvisor stack (proxy or raw mode)
// and dialed for real from this machine. No TUN device or local listener is involved.
func runExitMode(ctx context.Context, tun *tunnel.TCPTunnel, trans transport.Transport) {
	log.Printf("ready exit node (mode %s)", tun.ExitMode())
	startWatchdog(ctx, "exit", packetsRecv(trans), trans)
	<-ctx.Done()
}

// splitDomains splits a comma/space-separated site list into trimmed, de-duplicated domains.
func splitDomains(raw string) []string {
	var out []string
	seen := map[string]struct{}{}
	for _, part := range strings.FieldsFunc(raw, func(r rune) bool { return r == ',' || r == ' ' }) {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		if _, ok := seen[part]; ok {
			continue
		}
		seen[part] = struct{}{}
		out = append(out, part)
	}
	return out
}
