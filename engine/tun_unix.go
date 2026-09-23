//go:build unix

package main

import (
	"io"
	"os"
)

// tunFiles wraps a single inherited TUN fd: on Linux the same fd is both the read and the
// write side of the device. The Windows handle arguments are ignored here.
func tunFiles(fd, _readHandle, _writeHandle int) (r io.Reader, w io.Writer, cleanup func()) {
	f := os.NewFile(uintptr(fd), "tun")
	return f, f, func() { _ = f.Close() }
}
