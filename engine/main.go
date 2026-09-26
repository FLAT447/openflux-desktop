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
// Core features the shim can turn on: a selectable transport (--transport: yandex,
// yandex_multistream, volga, oneme, cupsonline, mailru, boards) with its wire codec
// (--codec, ignored by the self-compressing Yandex transport, which is always batched),
// multiple parallel WebSocket streams (--streams, multistream), per-domain split tunneling
// for TUN mode (--split-mode/--split-sites, exclusive or inclusive), and encrypted DNS
// upstreams (--dns accepts plain, tls://… for DoT and https://… for DoH).
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	"universal-bypass-tool/gateway"
	"universal-bypass-tool/socks5"
	"universal-bypass-tool/transport"
	"universal-bypass-tool/transport/cupsonline"
	"universal-bypass-tool/transport/mailru"
	"universal-bypass-tool/transport/oneme"
	"universal-bypass-tool/transport/yandex"
	"universal-bypass-tool/tunnel"
	"universal-bypass-tool/utils"
)

type cookieProvider interface {
	ProvideCookies(string)
}

func main() {
	url := flag.String("url", "", "document URL")
	docURLs := flag.String("urls", "", "comma-separated document URLs for yandex_multistream")
	transportType := flag.String("transport", "yandex", "transport type")
	codecName := flag.String("codec", "legacy", "wire codec")
	socksAddr := flag.String("socks5", "127.0.0.1:1080", "SOCKS5 listen address")
	tunFD := flag.Int("tun-fd", 0, "inherited TUN fd")
	tunRead := flag.Int("tun-read", 0, "inherited TUN read handle")
	tunWrite := flag.Int("tun-write", 0, "inherited TUN write handle")
	exit := flag.Bool("exit", false, "run as an exit node")
	exitNode := flag.Bool("exit-node", false, "run as an exit node")
	exitMode := flag.String("exit-mode", "raw", "exit-node mode")
	streams := flag.Int("streams", 1, "number of parallel streams")
	splitMode := flag.String("split-mode", "", "TUN split tunneling mode")
	splitSites := flag.String("split-sites", "", "comma-separated split-tunnel sites")
	dns := flag.String("dns", "77.88.8.8", "upstream DNS")
	sockMark := flag.Int("sock-mark", 0, "SO_MARK for transport sockets")
	token := flag.String("token", "", "e2e key token")
	mtu := flag.Uint("mtu", 0, "optional tunnel MTU override")
	debug := flag.Bool("debug", false, "verbose debug logging")
	var maxToken string
	var maxUID string
	var captchaMode string
	flag.StringVar(&maxToken, "max-token", "", "OneMe MAX token")
	flag.StringVar(&maxToken, "maxToken", "", "OneMe MAX token")
	flag.StringVar(&maxUID, "max-uid", "", "OneMe target user ID")
	flag.StringVar(&maxUID, "maxUid", "", "OneMe target user ID")
	flag.StringVar(&captchaMode, "captcha-solve-mode", "off", "Yandex CAPTCHA handling")
	flag.Parse()

	isExit := *exit || *exitNode
	if *streams < 1 {
		log.Fatalf("[ENGINE] -streams must be >= 1")
	}
	if !isExit && *tunFD <= 0 && *tunRead <= 0 && *socksAddr == "" {
		log.Fatalf("[ENGINE] either -tun-fd/--tun-read, -socks5, or -exit is required")
	}

	if *debug {
		utils.EnableDebug()
	}
	if *sockMark != 0 {
		installSockMarkProtector(*sockMark)
	}
	if *tunFD > 0 || *tunRead > 0 {
		net.DefaultResolver = transport.ProtectedResolver()
	}

	config := transport.DefaultConfig()
	trans, providers, err := buildTransports(
		*transportType,
		*url,
		*docURLs,
		*codecName,
		*token,
		maxToken,
		maxUID,
		captchaMode,
		isExit,
		*streams,
		config,
	)
	if err != nil {
		log.Fatalf("[ENGINE] %v", err)
	}
	watchCookieCommands(providers)
	if err := trans.Start(); err != nil {
		log.Fatalf("[ENGINE] start transport: %v", err)
	}

	em, err := tunnel.ParseExitMode(*exitMode)
	if err != nil {
		log.Fatalf("[ENGINE] bad -exit-mode %q: %v", *exitMode, err)
	}
	tun := tunnel.NewTCPTunnelMode(trans, isExit, em)
	if *mtu > 0 {
		tun.SetMTU(uint32(*mtu))
	}
	defer tun.Close()
	defer trans.Stop()

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	switch {
	case isExit:
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

func buildTransports(kind, docURL, rawDocURLs, codecName, token, maxToken, maxUID, captchaMode string, isExit bool, streamCount int, config transport.TransportConfig) (transport.Transport, []cookieProvider, error) {
	switch kind {
	case "yandex":
		if streamCount <= 1 {
			tr, provider, err := buildTransport(kind, docURL, codecName, token, maxToken, maxUID, captchaMode, isExit, 0, false, config)
			return tr, providerSlice(provider), err
		}
		streams := make([]transport.Transport, streamCount)
		providers := make([]cookieProvider, 0, streamCount)
		for i := range streams {
			tr, provider, err := buildTransport(kind, docURL, codecName, token, maxToken, maxUID, captchaMode, isExit, i, i > 0, config)
			if err != nil {
				return nil, nil, err
			}
			streams[i] = tr
			if provider != nil {
				providers = append(providers, provider)
			}
		}
		log.Printf("[ENGINE] multistream: %d streams", streamCount)
		return transport.NewMultiStreamTransport(streams), providers, nil
	case "yandex_multistream":
		urls := splitValues(rawDocURLs)
		if len(urls) < 2 {
			return nil, nil, fmt.Errorf("--transport yandex_multistream requires --urls with 2+ comma-separated document URLs")
		}
		streams := make([]transport.Transport, len(urls))
		providers := make([]cookieProvider, 0, len(urls))
		for i, url := range urls {
			tr, provider, err := buildTransport(kind, url, codecName, token, maxToken, maxUID, captchaMode, isExit, i, true, config)
			if err != nil {
				return nil, nil, err
			}
			streams[i] = tr
			if provider != nil {
				providers = append(providers, provider)
			}
		}
		log.Printf("[ENGINE] yandex multistream: %d document streams", len(urls))
		return transport.NewMultiStreamTransport(streams), providers, nil
	default:
		if streamCount != 1 {
			log.Printf("[ENGINE] ignoring --streams=%d for transport %s", streamCount, kind)
		}
		tr, provider, err := buildTransport(kind, docURL, codecName, token, maxToken, maxUID, captchaMode, isExit, 0, false, config)
		return tr, providerSlice(provider), err
	}
}

func buildTransport(kind, docURL, codecName, token, maxToken, maxUID, captchaMode string, isExit bool, streamIndex int, perStreamKey bool, config transport.TransportConfig) (transport.Transport, cookieProvider, error) {
	var inner transport.Transport
	setEvents := func(tr transport.Transport) {
		tr.SetEventCallback(func(code, detail string) {
			label := ""
			if streamIndex > 0 {
				label = fmt.Sprintf(" (stream %d)", streamIndex)
			}
			log.Printf("[TUNNEL] event %s%s (attempt %s)", code, label, detail)
		})
	}
	wrapCodec := func(tr transport.Transport) (transport.Transport, error) {
		return transport.WrapCodec(tr, codecName)
	}

	switch kind {
	case "yandex", "yandex_multistream":
		if strings.TrimSpace(docURL) == "" {
			return nil, nil, fmt.Errorf("transport %s requires a document URL", kind)
		}
		yd := yandex.NewYandexDocsTransport(docURL, config)
		yd.SetCaptchaSolveMode(parseCaptchaSolveMode(captchaMode))
		setEvents(yd)
		if token != "" {
			if perStreamKey {
				yd.EnableEncryptedSelfCompressionForStream(token, isExit, streamIndex)
			} else {
				yd.EnableEncryptedSelfCompression(token, isExit)
			}
		} else {
			yd.EnableSelfCompression()
		}
		return yd, yd, nil
	case "boards":
		if strings.TrimSpace(docURL) == "" {
			return nil, nil, fmt.Errorf("transport boards requires a document URL")
		}
		yd := yandex.NewBoardsTransport(docURL, config)
		setEvents(yd)
		return transport.NewBatchedTransport(yd), yd, nil
	case "volga":
		if strings.TrimSpace(docURL) == "" {
			return nil, nil, fmt.Errorf("transport volga requires a document URL")
		}
		inner = yandex.NewYandexVolgaTransport(docURL, config)
		setEvents(inner)
		wrapped, err := wrapCodec(inner)
		return wrapped, nil, err
	case "oneme":
		uid, err := strconv.ParseInt(maxUID, 10, 64)
		if err != nil || uid <= 0 {
			return nil, nil, fmt.Errorf("oneme requires a positive numeric --max-uid")
		}
		if strings.TrimSpace(maxToken) == "" {
			return nil, nil, fmt.Errorf("oneme requires --max-token")
		}
		inner = oneme.NewOneMeTransport(isExit, maxToken, uid, config)
		setEvents(inner)
		wrapped, err := wrapCodec(inner)
		return wrapped, nil, err
	case "cupsonline":
		if strings.TrimSpace(docURL) == "" {
			return nil, nil, fmt.Errorf("transport cupsonline requires a document URL")
		}
		inner = cupsonline.NewCupsonlineTransport(docURL, config, !isExit)
		setEvents(inner)
		wrapped, err := wrapCodec(inner)
		return wrapped, nil, err
	case "mailru":
		if strings.TrimSpace(docURL) == "" {
			return nil, nil, fmt.Errorf("transport mailru requires a document URL")
		}
		inner = mailru.NewMailruDocsTransport(docURL, config)
		setEvents(inner)
		wrapped, err := wrapCodec(inner)
		return wrapped, nil, err
	default:
		return nil, nil, fmt.Errorf("unknown transport type %q", kind)
	}
}

func providerSlice(provider cookieProvider) []cookieProvider {
	if provider == nil {
		return nil
	}
	return []cookieProvider{provider}
}

func splitValues(raw string) []string {
	parts := strings.Split(raw, ",")
	values := make([]string, 0, len(parts))
	for _, part := range parts {
		if value := strings.TrimSpace(part); value != "" {
			values = append(values, value)
		}
	}
	return values
}

func watchCookieCommands(providers []cookieProvider) {
	if len(providers) == 0 {
		return
	}
	go func() {
		scanner := bufio.NewScanner(os.Stdin)
		scanner.Buffer(make([]byte, 4096), 1024*1024)
		for scanner.Scan() {
			var command struct {
				Cmd       string `json:"cmd"`
				CookieStr string `json:"cookie_str"`
			}
			if err := json.Unmarshal(scanner.Bytes(), &command); err != nil || command.Cmd != "ProvideCookies" {
				continue
			}
			for _, provider := range providers {
				provider.ProvideCookies(command.CookieStr)
			}
		}
	}()
}

func parseCaptchaSolveMode(value string) yandex.CaptchaSolveMode {
	if value == "headless_browser" {
		return yandex.CaptchaSolveModeHeadlessBrowser
	}
	return yandex.CaptchaSolveModeOff
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
