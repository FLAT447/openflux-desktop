package gateway

import "encoding/binary"

// sniServerName is deliberately defensive: every offset is bounds-checked so a malformed or malicious prefix can never panic.
func sniServerName(p []byte) string {
	if len(p) < 5 {
		return ""
	}
	// Content type 0x16 = handshake; we don't care about the record version.
	if p[0] != 0x16 {
		return ""
	}
	recordLen := int(binary.BigEndian.Uint16(p[3:5]))
	body := p[5:]
	if len(body) > recordLen {
		body = body[:recordLen]
	}

	// Handshake header: type + 24-bit length.
	if len(body) < 4 {
		return ""
	}
	if body[0] != 0x01 { // client_hello
		return ""
	}
	hsLen := int(body[1])<<16 | int(body[2])<<8 | int(body[3])
	hello := body[4:]
	if len(hello) > hsLen {
		hello = hello[:hsLen]
	}

	// ClientHello: legacy_version (2) + random (32) + session id.
	off := 2 + 32
	if len(hello) < off+1 {
		return ""
	}
	sidLen := int(hello[off])
	off++
	if len(hello) < off+sidLen+2 {
		return ""
	}
	off += sidLen

	// Cipher suites.
	csLen := int(binary.BigEndian.Uint16(hello[off : off+2]))
	off += 2
	if len(hello) < off+csLen+1 {
		return ""
	}
	off += csLen

	// Compression methods.
	cmLen := int(hello[off])
	off++
	if len(hello) < off+cmLen+2 {
		return ""
	}
	off += cmLen

	// Extensions.
	extLen := int(binary.BigEndian.Uint16(hello[off : off+2]))
	off += 2
	end := off + extLen
	if end > len(hello) {
		end = len(hello)
	}

	for off+4 <= end {
		extType := binary.BigEndian.Uint16(hello[off : off+2])
		extLen := int(binary.BigEndian.Uint16(hello[off+2 : off+4]))
		off += 4
		if off+extLen > end {
			return ""
		}
		if extType == 0x0000 { // server_name
			return parseSNIExtension(hello[off : off+extLen])
		}
		off += extLen
	}
	return ""
}

func parseSNIExtension(p []byte) string {
	if len(p) < 2 {
		return ""
	}
	listLen := int(binary.BigEndian.Uint16(p[:2]))
	if listLen > len(p)-2 {
		listLen = len(p) - 2
	}
	p = p[2 : 2+listLen]

	for len(p) >= 3 {
		nameType := p[0]
		nameLen := int(binary.BigEndian.Uint16(p[1:3]))
		p = p[3:]
		if nameLen > len(p) {
			return ""
		}
		if nameType == 0x00 { // host_name
			return string(p[:nameLen])
		}
		p = p[nameLen:]
	}
	return ""
}
