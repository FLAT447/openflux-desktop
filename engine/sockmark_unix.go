//go:build unix

package main

import (
	"log"
	"os"

	"golang.org/x/sys/unix"

	"universal-bypass-tool/transport"
	"universal-bypass-tool/utils"
)

// installSockMarkProtector marks the transport's own sockets with SO_MARK, so the TUN-mode
// fwmark routing rule (fwmark 0x2547 -> main table) keeps them out of the tunnel and on
// the physical link.
func installSockMarkProtector(sockMark int) {
	if err := probeSockMark(sockMark); err != nil {
		// Without the mark, the transport's own bootstrap traffic gets captured by the very
		// tunnel it is trying to establish, and the only symptom is a connection that never
		// carries a packet. Report it unconditionally (not behind -debug): the usual cause is
		// the installed engine having lost its cap_net_admin capability.
		log.Printf("[ENGINE] cannot set SO_MARK %#x on transport sockets: %v", sockMark, err)
		log.Printf("[ENGINE] TUN mode will not connect until that is fixed; re-install so the")
		log.Printf("[ENGINE] engine keeps its capability: setcap cap_net_admin+ep %s", engineBinaryPath())
	}
	transport.SetProtector(func(fd int) bool {
		if err := unix.SetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_MARK, sockMark); err != nil {
			utils.Debugf("[ENGINE] SO_MARK %#x on fd %d failed: %v", sockMark, fd, err)
		} else {
			utils.Debugf("[ENGINE] SO_MARK %#x on fd %d ok", sockMark, fd)
		}
		return true
	})
}

// probeSockMark reports whether this process is allowed to set SO_MARK, which requires
// CAP_NET_ADMIN (granted to the installed engine as a file capability).
func probeSockMark(sockMark int) error {
	fd, err := unix.Socket(unix.AF_INET, unix.SOCK_DGRAM, 0)
	if err != nil {
		return err
	}
	defer unix.Close(fd)
	return unix.SetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_MARK, sockMark)
}

// engineBinaryPath only exists to make the warning above actionable.
func engineBinaryPath() string {
	if exe, err := os.Executable(); err == nil {
		return exe
	}
	return "/usr/local/lib/openflux/openflux-engine"
}
