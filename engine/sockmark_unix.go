//go:build unix

package main

import (
	"golang.org/x/sys/unix"

	"universal-bypass-tool/transport"
	"universal-bypass-tool/utils"
)

// installSockMarkProtector marks the transport's own sockets with SO_MARK, so the TUN-mode
// fwmark routing rule (fwmark 0x2547 -> main table) keeps them out of the tunnel and on
// the physical link.
func installSockMarkProtector(sockMark int) {
	transport.SetProtector(func(fd int) bool {
		if err := unix.SetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_MARK, sockMark); err != nil {
			utils.Debugf("[ENGINE] SO_MARK %d on fd %d failed: %v", sockMark, fd, err)
		} else {
			utils.Debugf("[ENGINE] SO_MARK %d on fd %d ok", sockMark, fd)
		}
		return true
	})
}
