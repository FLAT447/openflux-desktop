# OpenFlux desktop client

**English** | [Русский](README.ru.md)

A desktop client for a [Yandex Docs](https://yandex.ru/dev/docs/)–style controlplane
("docs-as-congress" tunnel config). One manager process (`openflux`) and a Go engine
(`openflux-engine`) give you:

- **SOCKS5** client mode (`connect` / GUI Connect) — the classic tunnel entry point;
- **system proxy** toggle — point GNOME/KDE proxy settings at the local SOCKS5 listener;
- **TUN mode** — full-system capture through a policy-routed interface (fwmark keeps the
  engine's own sockets off its tunnel on Linux, its own `/1` routes on Windows);
- **multistream** — 1–8 parallel WebSocket streams per session;
- **split tunneling** — `exclude` (listed sites bypass the tunnel) or `include` (only
  listed sites use it);
- **encrypted DNS** for the TUN gateway — plain `ip[:port]`, `tls://host` (DoT) or
  `https://host/path` (DoH);
- **exit-node mode** — run this host as the far end of the tunnel for other peers;
- **transport selection** — `yandex`, `volga`, `oneme`, `yandex_multistream`,
  `cupsonline`, `mailru` and experimental `boards`, with `legacy`/`batched` codecs
  for the generic transports.

Three frontends share one implementation: the CLI, the TUI (`openflux tui`) and the
[Tauri](https://tauri.app) GUI (`openflux-gui`) all call the same `openflux::actions`
library, so they can never drift apart.

## Layout

```
src/        Rust library + CLI/TUI, platform TUN backends (src/tun/)
gui/        Tauri v2 desktop GUI (static frontend, embedded at build time)
engine/     Go network engine (WebSocket tunnel, streams, split, DoT/DoH, exit node)
packaging/  install/uninstall scripts and .desktop file
```

## Core

This client talks to and tunnels through the OpenFlux server:

[**wlruscfd/openflux-server**](https://github.com/wlruscfd/openflux-server) — the
controlplane and exit/tunnel infrastructure the client connects to.

## Building

Linux (needs the Rust/Go toolchains, plus for the GUI: `webkit2gtk-4.1`,
`librsvg`, `dbus` and X dev headers):

```sh
make            # cargo build --release + go build, copies launcher to bin/
make gui        # builds gui/target/release/openflux-gui
make lint       # cargo clippy -D warnings (both crates) + go vet
make test       # cargo test (CLI + GUI) + go test (engine)
```

Engine install (grants `cap_net_admin` so TUN's fwmark rule works; `TUN` needs root):

```sh
make install    # pkexec
make install-gui
```

Release bundles:

```sh
make dist GUI=1            # Linux tarball + zip into dist/
make dist-windows GUI=1    # Windows build + bundled wintun.dll (needs mingw-w64 + rustup target)
```

## Usage

```sh
openflux import  <docs-url-or-link>        # pull a profile from the controlplane
openflux add-profile demo --transport yandex --doc-url <url> --streams 2
openflux add-profile multi --transport yandex_multistream \
    --doc-urls <url-1>,<url-2> --streams 2
openflux add-profile max --transport oneme \
    --max-token <token> --max-uid <uid> --streams 1
openflux add-profile flaky --transport yandex --doc-url <url> \
    --captcha-solve-mode headless_browser
openflux edit-profile demo --streams 3
openflux settings show                     # SOCKS5 port, DNS, split tunneling
openflux settings set --socks-port 1080 --dns tls://1.1.1.1 \
    --split-mode exclude --split-domains '*.ya.ru'
openflux connect                           # SOCKS5 client mode
openflux proxy on                          # system proxy -> SOCKS5
openflux tun on                            # TUN mode (pkexec; Windows: elevated)
openflux exit on|off                       # run this host as an exit node
openflux status | logs | tui
```

The SOCKS5 port, the TUN DNS upstream and split tunneling are machine-wide settings
(`openflux settings`, or the "⚙" button in the GUI), not profile fields: they apply to every
profile at once. Streams, MTU and the captcha mode stay per profile.

`yandex_multistream` requires at least two document URLs. `oneme` requires a positive
numeric `--max-uid` and `--max-token`. The controlplane currently exposes managed
transports `yandex`, `yandex_multistream`, `mailru` and `boards`; the other transports
can be configured manually from the CLI or GUI.

`--captcha-solve-mode headless_browser` (Yandex transports only) lets the engine open
the document in a local headless Chrome and solve the bot check itself, which needs a
Chrome/Chromium install; the default `off` just reports the block in the engine log.

Inside the TUI: `c` connect, `d` disconnect, `p` proxy, `t` TUN (suspends for polkit),
`e` exit node, `Enter` activate profile, `q` quit.

The GUI exposes the same actions; the TUN button on Windows re-invokes the binary
elevated (`ShellExecuteW runas openflux-gui tun on|off`, UAC-gated).

## Configuration & state

- Config lives in `~/.config/openflux/openflux.toml` (override with `OPENFLUX_CONFIG_DIR`).
- Engine state (pidfiles, engine log, proxy snapshot) is written alongside it.
- Profiles are edited on the CLI or in the GUI; keys are the controlplane's shared-link
  tokens. Never commit real keys or tokens.

## Platforms

| Feature        | Linux                       | Windows                         |
|----------------|-----------------------------|---------------------------------|
| TUN            | `/dev/net/tun` fd + fwmark  | Wintun adapter + netsh `/1`     |
| Privileged ops | `pkexec`                    | UAC elevation (`runas`)         |
| Exit node      | `raw` (needs root)          | `proxy` (TCP-only, no root)     |

Windows TUN requires WireGuard's Wintun 0.14 (`wintun.dll`) beside the executable; the
Windows path is compile-verified but not yet exercised on a real Windows host.

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).