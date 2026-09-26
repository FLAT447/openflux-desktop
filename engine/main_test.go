package main

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"universal-bypass-tool/transport"
	"universal-bypass-tool/transport/yandex"
)

func TestBuildTransportsYandexMultistream(t *testing.T) {
	trans, providers, err := buildTransports(
		"yandex_multistream",
		"",
		"https://docs.yandex.ru/i/a,https://docs.yandex.ru/i/b",
		"legacy",
		"token",
		"",
		"",
		"off",
		false,
		2,
		transport.DefaultConfig(),
	)
	if err != nil {
		t.Fatalf("buildTransports returned error: %v", err)
	}
	if _, ok := trans.(*transport.MultiStreamTransport); !ok {
		t.Fatalf("transport = %T, want *transport.MultiStreamTransport", trans)
	}
	if len(providers) != 2 {
		t.Fatalf("providers = %d, want 2", len(providers))
	}
}

func TestBuildTransportsOneMeCredentials(t *testing.T) {
	trans, providers, err := buildTransports(
		"oneme",
		"",
		"",
		"batched",
		"",
		"max-token",
		"42",
		"off",
		false,
		1,
		transport.DefaultConfig(),
	)
	if err != nil {
		t.Fatalf("buildTransports returned error: %v", err)
	}
	if trans == nil {
		t.Fatal("transport is nil")
	}
	if len(providers) != 0 {
		t.Fatalf("providers = %d, want 0", len(providers))
	}
}

func TestBuildTransportsRejectsInvalidOneMeUID(t *testing.T) {
	_, _, err := buildTransports(
		"oneme",
		"",
		"",
		"legacy",
		"",
		"max-token",
		"not-a-number",
		"off",
		false,
		1,
		transport.DefaultConfig(),
	)
	if err == nil {
		t.Fatal("buildTransports accepted an invalid OneMe UID")
	}
}

func TestBuildTransportsRejectsShortMultistream(t *testing.T) {
	_, _, err := buildTransports(
		"yandex_multistream",
		"",
		"https://docs.yandex.ru/i/a",
		"legacy",
		"",
		"",
		"",
		"off",
		false,
		1,
		transport.DefaultConfig(),
	)
	if err == nil {
		t.Fatal("buildTransports accepted fewer than two document URLs")
	}
}

func TestBuildTransportsRequiresDocURL(t *testing.T) {
	for _, kind := range []string{"yandex", "volga", "boards", "cupsonline", "mailru"} {
		_, _, err := buildTransports(kind, "", "", "legacy", "", "", "", "off", false, 1, transport.DefaultConfig())
		if err == nil {
			t.Errorf("buildTransports accepted %s without a document URL", kind)
		}
	}
}

func TestBuildTransportsRejectsUnknownTransportAndCodec(t *testing.T) {
	if _, _, err := buildTransports("nope", "https://docs.yandex.ru/i/a", "", "legacy", "", "", "", "off", false, 1, transport.DefaultConfig()); err == nil {
		t.Error("buildTransports accepted an unknown transport")
	}
	if _, _, err := buildTransports("volga", "https://docs.yandex.ru/i/a", "", "zstd", "", "", "", "off", false, 1, transport.DefaultConfig()); err == nil {
		t.Error("buildTransports accepted an unknown codec")
	}
}

func TestBuildTransportsYandexMultistreamUsesPerStreamKeys(t *testing.T) {
	trans, providers, err := buildTransports(
		"yandex_multistream",
		"",
		"https://docs.yandex.ru/i/a,https://docs.yandex.ru/i/b,https://docs.yandex.ru/i/c",
		"legacy",
		"token",
		"",
		"",
		"off",
		false,
		3,
		transport.DefaultConfig(),
	)
	if err != nil {
		t.Fatalf("buildTransports returned error: %v", err)
	}
	if _, ok := trans.(*transport.MultiStreamTransport); !ok {
		t.Fatalf("transport = %T, want *transport.MultiStreamTransport", trans)
	}
	if len(providers) != 3 {
		t.Fatalf("providers = %d, want 3 (one per document stream)", len(providers))
	}
}

func TestParseCaptchaSolveMode(t *testing.T) {
	if got := parseCaptchaSolveMode("headless_browser"); got != yandex.CaptchaSolveModeHeadlessBrowser {
		t.Errorf("parseCaptchaSolveMode(headless_browser) = %v", got)
	}
	for _, value := range []string{"", "off", "bogus"} {
		if got := parseCaptchaSolveMode(value); got != yandex.CaptchaSolveModeOff {
			t.Errorf("parseCaptchaSolveMode(%q) = %v, want off", value, got)
		}
	}
}

// Every transport must keep the construction shape upstream uses, otherwise the wire
// format silently diverges: the service (and the server-side exit node) always wraps
// boards in the batched transport, mailru/volga/oneme/cupsonline in the codec wrapper,
// and the Yandex Docs transports in their own compression/encryption stack.
// See upstream main.go:112-135, mobile/mobile.go:343-358 and
// nodeagent/orchestrator.go:385-420.
func TestBuildTransportsWrappingPerTransport(t *testing.T) {
	cases := []struct {
		kind         string
		docURL       string
		rawDocURLs   string
		maxToken     string
		maxUID       string
		codec        string
		wantType     string
		wantProvider bool
	}{
		{kind: "yandex", docURL: "https://disk.yandex.ru/i/a", wantType: "*yandex.YandexDocsTransport", wantProvider: true},
		{kind: "volga", docURL: "https://disk.yandex.ru/i/a", wantType: "*transport.CompressedTransport", wantProvider: false},
		{kind: "mailru", docURL: "https://docs.mail.ru/i/a", wantType: "*transport.CompressedTransport", wantProvider: false},
		{kind: "oneme", maxToken: "max-token", maxUID: "42", wantType: "*transport.CompressedTransport", wantProvider: false},
		{kind: "cupsonline", docURL: "https://cupsonline.online/i/a", wantType: "*transport.CompressedTransport", wantProvider: false},
		// boards is always batched, so --codec must not add a second wrapper.
		{kind: "boards", docURL: "https://boards.yandex.ru/i/a", wantType: "*transport.BatchedTransport", wantProvider: true},
		{kind: "yandex_multistream", rawDocURLs: "https://docs.yandex.ru/i/a,https://docs.yandex.ru/i/b", wantType: "*transport.MultiStreamTransport", wantProvider: true},
	}

	for _, tc := range cases {
		t.Run(tc.kind, func(t *testing.T) {
			trans, providers, err := buildTransports(
				tc.kind, tc.docURL, tc.rawDocURLs, tc.codec, "", tc.maxToken, tc.maxUID, "off", false, 1, transport.DefaultConfig(),
			)
			if err != nil {
				t.Fatalf("buildTransports returned error: %v", err)
			}
			if got := fmt.Sprintf("%T", trans); got != tc.wantType {
				t.Errorf("transport = %s, want %s", got, tc.wantType)
			}
			if tc.wantProvider && len(providers) == 0 {
				t.Error("transport must expose a cookie provider")
			}
			if !tc.wantProvider && len(providers) != 0 {
				t.Errorf("providers = %d, want 0 (no cookie protocol for this transport)", len(providers))
			}
		})
	}
}

// The desktop default codec must stay wire-compatible with the server-side exit node,
// which hardcodes transport.NewCompressedTransport for mailru.
func TestBuildTransportsMailruDefaultCodecMatchesServerSide(t *testing.T) {
	trans, _, err := buildTransports("mailru", "https://docs.mail.ru/i/a", "", "legacy", "", "", "", "off", false, 1, transport.DefaultConfig())
	if err != nil {
		t.Fatalf("buildTransports returned error: %v", err)
	}
	if _, ok := trans.(*transport.CompressedTransport); !ok {
		t.Fatalf("transport = %T, want *transport.CompressedTransport", trans)
	}
	tr, _, err := buildTransports("mailru", "https://docs.mail.ru/i/a", "", "batched", "", "", "", "off", false, 1, transport.DefaultConfig())
	if err != nil {
		t.Fatalf("buildTransports returned error: %v", err)
	}
	if _, ok := tr.(*transport.BatchedTransport); !ok {
		t.Errorf("batched codec = %T, want *transport.BatchedTransport", tr)
	}
}

// mailru and boards have no encrypted-self-compression support upstream, so the desktop
// must never fall back to the Yandex transport for them, and a key token must be inert.
func TestBuildTransportsNonYandexIgnoreKeyToken(t *testing.T) {
	for _, tc := range []struct{ kind, docURL, wantInner string }{
		{kind: "mailru", docURL: "https://docs.mail.ru/i/a", wantInner: "*mailru.MailruDocsTransport"},
		{kind: "boards", docURL: "https://boards.yandex.ru/i/a", wantInner: "*yandex.BoardsTransport"},
		{kind: "volga", docURL: "https://disk.yandex.ru/i/a", wantInner: "*yandex.YandexVolgaTransport"},
		{kind: "cupsonline", docURL: "https://cupsonline.online/i/a", wantInner: "*cupsonline.CupsonlineTransport"},
	} {
		trans, _, err := buildTransports(tc.kind, tc.docURL, "", "legacy", "key_t_secret", "", "", "off", false, 1, transport.DefaultConfig())
		if err != nil {
			t.Fatalf("buildTransports(%s) returned error: %v", tc.kind, err)
		}
		inner := peelWrappers(trans)
		if got := fmt.Sprintf("%T", inner); got != tc.wantInner {
			t.Errorf("%s inner transport = %s, want %s", tc.kind, got, tc.wantInner)
		}
		if _, isYandex := inner.(*yandex.YandexDocsTransport); isYandex {
			t.Errorf("%s must not fall back to the Yandex Docs transport", tc.kind)
		}
	}
}

// TestVendoredTransportsEscapeTheTunnel guards a real TUN-mode deadlock: the engine
// installs a socket protector (--sock-mark) that only applies to sockets built by
// transport.ProtectedDialer, and a transport that dials out with a plain net.Dialer or
// http.Client has its own bootstrap request swallowed by the tunnel it is bringing up.
// Re-vendoring upstream drops the local fix silently, so assert it at the source level.
func TestVendoredTransportsEscapeTheTunnel(t *testing.T) {
	root := filepath.Join("vendor", "universal-bypass-tool", "transport")
	checked := 0
	err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() || !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "_test.go") {
			return nil
		}
		src, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		body := string(src)
		dialsOut := strings.Contains(body, "websocket.Dialer{") ||
			strings.Contains(body, "&http.Client{") ||
			strings.Contains(body, "&net.Dialer{") ||
			strings.Contains(body, "net.Dial(")
		if !dialsOut {
			return nil
		}
		rel, _ := filepath.Rel(root, path)
		// A file that forwards an injected http.RoundTripper (the captcha helper) borrows
		// the caller's already-protected transport, so it owns no socket of its own.
		if strings.Contains(body, "http.RoundTripper") {
			return nil
		}
		checked++
		if !strings.Contains(body, "ProtectedDialer") && !strings.Contains(body, "ProtectedResolver") {
			t.Errorf("%s dials out without a protected dialer: its traffic would be captured by the tunnel it establishes", rel)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("walk %s: %v", root, err)
	}
	if checked == 0 {
		t.Fatalf("no vendored transport files matched; the guard would pass vacuously")
	}
	t.Logf("checked %d vendored transport files", checked)
}

// peelWrappers strips the codec/batched wrappers to expose the transport implementation.
func peelWrappers(tr transport.Transport) transport.Transport {
	for {
		switch v := tr.(type) {
		case *transport.CompressedTransport:
			tr = v.Transport
		case *transport.BatchedTransport:
			tr = v.Transport
		default:
			return tr
		}
	}
}
