package gateway

import (
	"bytes"
	"encoding/binary"
	"net"
	"time"
)

const maxSniffBytes = 2048

const peekTimeout = 750 * time.Millisecond

type prependConn struct {
	net.Conn
	prefix *bytes.Reader
}

func (pc *prependConn) Read(p []byte) (int, error) {
	if pc.prefix.Len() > 0 {
		return pc.prefix.Read(p)
	}
	return pc.Conn.Read(p)
}

// sniffClientHello reads and replays the connection's head; the caller still owns conn's lifecycle after the read deadline is cleared.
func sniffClientHello(conn net.Conn) ([]byte, net.Conn) {
	deadline := time.Now().Add(peekTimeout)
	conn.SetReadDeadline(deadline)
	collected := make([]byte, 0, maxSniffBytes)
	buf := make([]byte, 512)

	for {
		n, err := conn.Read(buf)
		collected = append(collected, buf[:n]...)
		if err != nil {
			break
		}
		if len(collected) < 5 || collected[0] != 0x16 {
			break
		}
		recordLen := int(binary.BigEndian.Uint16(collected[3:5]))
		if len(collected) >= 5+recordLen {
			break
		}
		if len(collected) >= maxSniffBytes || time.Now().After(deadline) {
			break
		}
	}
	conn.SetReadDeadline(time.Time{})

	if len(collected) == 0 {
		return nil, conn
	}
	return collected, &prependConn{Conn: conn, prefix: bytes.NewReader(collected)}
}
