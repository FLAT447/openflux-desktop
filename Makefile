.PHONY: all build engine cargo gui install install-gui setcap dist dist-windows test lint

all: build

# Rust CLI + Go engine.
build: cargo engine
	@mkdir -p bin
	@cp target/release/openflux bin/openflux

cargo:
	cargo build --release --locked

# Tauri v2 GUI (separate crate under gui/; needs webkit2gtk-4.1, librsvg, dbus and
# X dev headers installed). Produces gui/target/release/openflux-gui.
gui:
	cargo build --release --locked --manifest-path gui/Cargo.toml

engine:
	cd engine && go build -mod=vendor -o openflux-engine .

# Install the engine to a non-nosuid prefix and grant it CAP_NET_ADMIN (needed for TUN
# mode's SO_MARK: the fwmark rule keeps the engine's own sockets out of its tunnel). Uses
# pkexec for the privileged steps.
PREFIX ?= /usr/local/lib/openflux
install: build
	pkexec sh -c 'mkdir -p $(PREFIX) && cp "$(CURDIR)/engine/openflux-engine" $(PREFIX)/openflux-engine && chmod +x $(PREFIX)/openflux-engine && setcap cap_net_admin+ep $(PREFIX)/openflux-engine && getcap $(PREFIX)/openflux-engine'

setcap:
	pkexec setcap cap_net_admin+ep $(PREFIX)/openflux-engine

# Install the GUI binary plus a launcher entry, hicolor icon, and the openflux://
# deep-link handler registration.
install-gui: gui
	pkexec sh -c 'cp "$(CURDIR)/gui/target/release/openflux-gui" /usr/local/bin/openflux-gui && chmod +x /usr/local/bin/openflux-gui && mkdir -p /usr/share/icons/hicolor/512x512/apps /usr/share/applications && cp "$(CURDIR)/gui/icons/icon.png" /usr/share/icons/hicolor/512x512/apps/openflux.png && cp "$(CURDIR)/packaging/openflux.desktop" /usr/share/applications/openflux.desktop && update-desktop-database /usr/share/applications 2>/dev/null || true'
	xdg-mime default openflux.desktop x-scheme-handler/openflux 2>/dev/null || true

# Distributable release: tarball (Linux). Binaries are laid out flat so the engine
# and the CLI/gui sit next to each other and the launcher's relative paths hold.
VERSION ?= 0.1.0
DIST_NAME := openflux-$(VERSION)-linux-$(shell uname -m)
GUI ?= 0
dist: cargo engine
	@if [ "$(GUI)" = 1 ]; then cd gui && cargo build --release --locked; fi
	@mkdir -p bin
	@cp -f target/release/openflux bin/openflux
	@if [ "$(GUI)" = 1 ]; then cp -f gui/target/release/openflux-gui bin/openflux-gui; fi
	@rm -rf dist/$(DIST_NAME) dist/$(DIST_NAME).tar.gz
	@mkdir -p dist/$(DIST_NAME)
	@cp -f bin/openflux dist/$(DIST_NAME)/
	@cp -f engine/openflux-engine dist/$(DIST_NAME)/
	@if [ -f bin/openflux-gui ]; then cp -f bin/openflux-gui dist/$(DIST_NAME)/; fi
	@cp -f packaging/install.sh packaging/uninstall.sh packaging/openflux.desktop dist/$(DIST_NAME)/
	@cp -f packaging/pre_install.sh dist/$(DIST_NAME)/ 2>/dev/null || true
	@cp -f gui/icons/icon.png dist/$(DIST_NAME)/icon.png
	@if [ -f README.md ]; then cp -f README.md dist/$(DIST_NAME)/; fi
	@if [ -f README.ru.md ]; then cp -f README.ru.md dist/$(DIST_NAME)/; fi
	@if [ -f LICENSE ]; then cp -f LICENSE dist/$(DIST_NAME)/; fi
	@chmod +x dist/$(DIST_NAME)/openflux dist/$(DIST_NAME)/openflux-engine \
	  dist/$(DIST_NAME)/install.sh dist/$(DIST_NAME)/uninstall.sh
	@if [ -f dist/$(DIST_NAME)/openflux-gui ]; then chmod +x dist/$(DIST_NAME)/openflux-gui; fi
	@cd dist && tar czf $(DIST_NAME).tar.gz $(DIST_NAME)
	@echo "dist ready: dist/$(DIST_NAME).tar.gz"

# Windows release: cross-compiled CLI (+ optional GUI) and the Go engine, plus WireGuard's
# wintun.dll which must sit next to the executable (the TUN launcher loads it at runtime).
# Requires a mingw-w64 cross toolchain: apt install gcc-mingw-w64-x86-64 (linker), and
# rustup target add x86_64-pc-windows-gnu. Network needed only to fetch wintun.dll.
WIN_TARGET := x86_64-pc-windows-gnu
DIST_NAME_WIN := openflux-$(VERSION)-windows-x86_64
WINTUN_URL ?= https://www.wintun.net/builds/wintun-0.14.1.zip
dist-windows: cargo-windows engine-windows
	@if [ "$(GUI)" = 1 ]; then $(MAKE) gui-windows; fi
	@mkdir -p dist/$(DIST_NAME_WIN)
	@cp -f target/$(WIN_TARGET)/release/openflux.exe dist/$(DIST_NAME_WIN)/openflux.exe
	@cp -f engine/openflux-engine.exe dist/$(DIST_NAME_WIN)/openflux-engine.exe
	@if [ -f bin/openflux-gui.exe ]; then cp -f bin/openflux-gui.exe dist/$(DIST_NAME_WIN)/openflux-gui.exe; fi
	@cp -f packaging/install.cmd packaging/uninstall.cmd dist/$(DIST_NAME_WIN)/
	@if [ -f README.md ]; then cp -f README.md dist/$(DIST_NAME_WIN)/; fi
	@if [ -f README.ru.md ]; then cp -f README.ru.md dist/$(DIST_NAME_WIN)/; fi
	@if [ -f LICENSE ]; then cp -f LICENSE dist/$(DIST_NAME_WIN)/; fi
	@mkdir -p dist/$(DIST_NAME_WIN)/bin/amd64
	@if [ -f wintun.dll ] ; then \
		echo "wintun.dll: local copy found"; \
	elif [ -f dist/$(DIST_NAME_WIN)/wintun.dll ]; then \
		echo "wintun.dll: already bundled"; \
	else \
		echo "wintun.dll: downloading $(WINTUN_URL)"; \
		curl -fSL -o /tmp/wintun.zip $(WINTUN_URL) && \
		unzip -jo /tmp/wintun.zip 'wintun/bin/amd64/wintun.dll' -d dist/$(DIST_NAME_WIN)/ && \
		rm -f /tmp/wintun.zip; \
	fi
	@cd dist/$(DIST_NAME_WIN) && zip -qr ../$(DIST_NAME_WIN).zip .
	@echo "dist-windows ready: dist/$(DIST_NAME_WIN).zip (needs wintun.dll bundled + an elevated install)"

cargo-windows:
	rustup target add $(WIN_TARGET) 2>/dev/null || true
	cargo build --release --locked --target $(WIN_TARGET)

engine-windows:
	cd engine && GOOS=windows CGO_ENABLED=0 go build -mod=vendor -o openflux-engine.exe .

gui-windows:
	cargo build --release --locked --target $(WIN_TARGET) --manifest-path gui/Cargo.toml
	@cp -f gui/target/$(WIN_TARGET)/release/openflux-gui.exe bin/openflux-gui.exe

test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings
	cd engine && go vet ./...
	cd gui && cargo clippy --all-targets -- -D warnings
