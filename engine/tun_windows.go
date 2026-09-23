//go:build windows

package main

import (
	"io"
	"os"
)

// tunFiles wraps the Wintun session handle the Rust side created. A Wintun session
// handle is a real file handle: packet I/O is implemented by the driver as plain
// ReadFile/WriteFile, so a single os.File wrapper serves as both the read and the write
// side (readHandle and writeHandle are always the same value in TUN mode). The Unix fd
// argument is ignored.
func tunFiles(_fd, readHandle, writeHandle int) (r io.Reader, w io.Writer, cleanup func()) {
	f := os.NewFile(uintptr(readHandle), "wintun")
	_ = writeHandle
	return f, f, func() { _ = f.Close() }
}
