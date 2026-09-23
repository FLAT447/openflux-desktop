//go:build windows

package main

// installSockMarkProtector is a no-op on Windows: SO_MARK / fwmark routing does not exist
// there, and TUN mode is not implemented yet, so SOCKS mode is the only path and the
// engine's sockets stay on the physical link by default. The -sock-mark flag is still
// accepted for a single cross-platform command line.
func installSockMarkProtector(_ int) {}
