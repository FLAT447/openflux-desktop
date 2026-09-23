package gateway

import (
	"encoding/binary"
	"net"
	"strings"
)

// Minimal DNS wire-format helpers - just enough to learn QNAME and A-record mappings from traffic already passing through relayDNS; not a full DNS implementation.

// isQuery reports whether the message has the QR bit clear (a query).
func isDNSQuery(msg []byte) bool {
	return len(msg) >= 4 && msg[2]&0x80 == 0
}

func skipDNSName(msg []byte, off int) (int, bool) {
	for {
		if off >= len(msg) {
			return 0, false
		}
		b := msg[off]
		switch {
		case b == 0:
			return off + 1, true
		case b&0xC0 == 0xC0:
			if off+1 >= len(msg) {
				return 0, false
			}
			return off + 2, true
		case b&0xC0 != 0:
			return 0, false
		default:
			if off+1+int(b) > len(msg) {
				return 0, false
			}
			off += 1 + int(b)
		}
	}
}

func readDNSName(msg []byte, off int) (string, int, bool) {
	var labels []string
	retOff := off
	seen := make(map[int]bool)
	jumps := 0

	for {
		if off >= len(msg) {
			return "", 0, false
		}
		b := msg[off]
		switch {
		case b == 0x00:
			if jumps == 0 {
				retOff = off + 1
			}
			name := strings.Join(labels, ".")
			if name == "" {
				name = "."
			}
			return name, retOff, true
		case b&0xC0 == 0xC0:
			if off+1 >= len(msg) {
				return "", 0, false
			}
			if jumps == 0 {
				retOff = off + 2
			}
			ptr := int(binary.BigEndian.Uint16(msg[off:off+2])) & 0x3FFF
			if ptr >= len(msg) || seen[ptr] {
				return "", 0, false
			}
			seen[ptr] = true
			jumps++
			if jumps > 16 {
				return "", 0, false
			}
			off = ptr
		case b&0xC0 != 0:
			return "", 0, false
		default:
			if off+1+int(b) > len(msg) {
				return "", 0, false
			}
			labels = append(labels, string(msg[off+1:off+1+int(b)]))
			off += 1 + int(b)
		}
	}
}

func dnsQuestionName(msg []byte) (string, bool) {
	if len(msg) < 12 || !isDNSQuery(msg) {
		return "", false
	}
	qd := binary.BigEndian.Uint16(msg[4:6])
	if qd == 0 {
		return "", false
	}
	name, off, ok := readDNSName(msg, 12)
	if !ok || off+4 > len(msg) {
		return "", false
	}
	return strings.TrimSuffix(name, "."), true
}

const (
	dnsTypeA       = 1
	dnsClassIN     = 1
	dnsMinTTLEntry = 10
	dnsMaxTTLEntry = 24 * 60 * 60
)

// dnsARecord holds one answer's owner name, its A IP and the record TTL.
type dnsARecord struct {
	domain string
	ip     string
	ttl    uint32
}

func dnsARecords(msg []byte) []dnsARecord {
	if len(msg) < 12 {
		return nil
	}
	qd := binary.BigEndian.Uint16(msg[4:6])
	an := binary.BigEndian.Uint16(msg[6:8])
	ns := binary.BigEndian.Uint16(msg[8:10])
	ar := binary.BigEndian.Uint16(msg[10:12])

	off := 12
	for i := uint16(0); i < qd; i++ {
		var ok bool
		off, ok = skipDNSName(msg, off)
		if !ok {
			return nil
		}
		if off+4 > len(msg) {
			return nil
		}
		off += 4 // qtype + qclass
	}

	total := int(an) + int(ns) + int(ar)
	var out []dnsARecord
	for i := 0; i < total; i++ {
		name, next, ok := readDNSName(msg, off)
		if !ok {
			return out
		}
		if next+10 > len(msg) {
			return out
		}
		rtype := binary.BigEndian.Uint16(msg[next : next+2])
		rclass := binary.BigEndian.Uint16(msg[next+2 : next+4])
		ttl := binary.BigEndian.Uint32(msg[next+4 : next+8])
		rdlen := int(binary.BigEndian.Uint16(msg[next+8 : next+10]))
		off = next + 10
		if off+rdlen > len(msg) {
			return out
		}
		if rclass == dnsClassIN && rtype == dnsTypeA && rdlen == 4 {
			ip := net.IP(msg[off : off+4]).String()
			out = append(out, dnsARecord{domain: strings.TrimSuffix(strings.ToLower(name), "."), ip: ip, ttl: clampTTL(ttl)})
		}
		off += rdlen
	}
	return out
}

func clampTTL(ttl uint32) uint32 {
	if ttl < dnsMinTTLEntry {
		return dnsMinTTLEntry
	}
	if ttl > dnsMaxTTLEntry {
		return dnsMaxTTLEntry
	}
	return ttl
}
