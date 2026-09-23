package gateway

import (
	"bufio"
	"crypto/tls"
	"fmt"
	"io"
	"net"
	"net/http"
	neturl "net/url"
	"strings"
	"time"
)

type dnsUpstreamKind int

const (
	dnsUpstreamPlain dnsUpstreamKind = iota // classic DNS-over-TCP (today's only behavior)
	dnsUpstreamDoT                          // DNS-over-TLS, RFC 7858
	dnsUpstreamDoH                          // DNS-over-HTTPS, RFC 8484 (wireformat)
)

type dnsUpstreamConfig struct {
	kind dnsUpstreamKind
	addr string // host:port to dial via Server.dialer.DialTCP
	host string // TLS ServerName / HTTP Host header (DoT/DoH only)
	path string // HTTP request path (DoH only)
}

// parseDNSUpstream accepts host[:port], tls://... (DoT), or https://... (DoH, wireformat) - the DNS exchange itself gains privacy, but still rides through the tunnel like everything else.
func parseDNSUpstream(raw string) dnsUpstreamConfig {
	if raw == "" {
		raw = "77.88.8.8:53"
	}

	switch {
	case strings.HasPrefix(raw, "tls://"):
		host := strings.TrimPrefix(raw, "tls://")
		hostname := host
		if h, _, err := net.SplitHostPort(host); err == nil {
			hostname = h
		} else {
			host = net.JoinHostPort(host, "853")
		}
		return dnsUpstreamConfig{kind: dnsUpstreamDoT, addr: host, host: hostname}

	case strings.HasPrefix(raw, "https://"):
		u, err := neturl.Parse(raw)
		if err != nil {
			return dnsUpstreamConfig{kind: dnsUpstreamPlain, addr: raw}
		}
		addr := u.Host
		if _, _, err := net.SplitHostPort(addr); err != nil {
			addr = net.JoinHostPort(addr, "443")
		}
		path := u.Path
		if path == "" {
			path = "/dns-query"
		}
		return dnsUpstreamConfig{kind: dnsUpstreamDoH, addr: addr, host: u.Hostname(), path: path}

	default:
		addr := raw
		if _, _, err := net.SplitHostPort(addr); err != nil {
			addr = net.JoinHostPort(addr, "53")
		}
		return dnsUpstreamConfig{kind: dnsUpstreamPlain, addr: addr}
	}
}

func (s *Server) queryUpstream(query []byte) ([]byte, error) {
	switch s.dnsUpstreamCfg.kind {
	case dnsUpstreamDoT:
		return s.queryDoT(query)
	case dnsUpstreamDoH:
		return s.queryDoH(query)
	default:
		return s.queryPlainDNS(query)
	}
}

func (s *Server) queryPlainDNS(query []byte) ([]byte, error) {
	conn, err := s.dialer.DialTCP(s.dnsUpstreamCfg.addr)
	if err != nil {
		return nil, fmt.Errorf("dial: %w", err)
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(dnsQueryTimeout))

	if err := writeDNSOverTCP(conn, query); err != nil {
		return nil, fmt.Errorf("write: %w", err)
	}
	return readDNSOverTCP(conn)
}

func (s *Server) dialUpstreamTLS() (*tls.Conn, error) {
	raw, err := s.dialer.DialTCP(s.dnsUpstreamCfg.addr)
	if err != nil {
		return nil, fmt.Errorf("dial: %w", err)
	}
	raw.SetDeadline(time.Now().Add(dnsQueryTimeout))

	conn := tls.Client(raw, s.dnsUpstreamTLSConfig)
	if err := conn.Handshake(); err != nil {
		raw.Close()
		return nil, fmt.Errorf("tls handshake: %w", err)
	}
	return conn, nil
}

func (s *Server) queryDoT(query []byte) ([]byte, error) {
	conn, err := s.dialUpstreamTLS()
	if err != nil {
		return nil, err
	}
	defer conn.Close()

	if err := writeDNSOverTCP(conn, query); err != nil {
		return nil, fmt.Errorf("write: %w", err)
	}
	return readDNSOverTCP(conn)
}

func (s *Server) queryDoH(query []byte) ([]byte, error) {
	if len(query) > maxDNSMessageSize {
		return nil, fmt.Errorf("dns message too large: %d bytes", len(query))
	}
	conn, err := s.dialUpstreamTLS()
	if err != nil {
		return nil, err
	}
	defer conn.Close()

	var req strings.Builder
	fmt.Fprintf(&req, "POST %s HTTP/1.1\r\n", s.dnsUpstreamCfg.path)
	fmt.Fprintf(&req, "Host: %s\r\n", s.dnsUpstreamCfg.host)
	req.WriteString("Content-Type: application/dns-message\r\n")
	req.WriteString("Accept: application/dns-message\r\n")
	fmt.Fprintf(&req, "Content-Length: %d\r\n", len(query))
	req.WriteString("Connection: close\r\n\r\n")

	if _, err := io.WriteString(conn, req.String()); err != nil {
		return nil, fmt.Errorf("write request: %w", err)
	}
	if _, err := conn.Write(query); err != nil {
		return nil, fmt.Errorf("write body: %w", err)
	}

	resp, err := http.ReadResponse(bufio.NewReader(conn), nil)
	if err != nil {
		return nil, fmt.Errorf("read response: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("doh status %d", resp.StatusCode)
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxDNSMessageSize))
	if err != nil {
		return nil, fmt.Errorf("read body: %w", err)
	}
	return body, nil
}
