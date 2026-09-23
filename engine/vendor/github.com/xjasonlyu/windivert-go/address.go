package windivert

import (
	"unsafe"
)

type Network struct {
	InterfaceIndex    uint32 /* Packet's interface index. */
	SubInterfaceIndex uint32 /* Packet's sub-interface index. */
	_                 [7]uint64
}

type Socket struct {
	EndpointID       uint64    /* Endpoint ID. */
	ParentEndpointID uint64    /* Parent endpoint ID. */
	ProcessID        uint32    /* Process ID. */
	LocalAddress     [16]uint8 /* Local address. */
	RemoteAddress    [16]uint8 /* Remote address. */
	LocalPort        uint16    /* Local port. */
	RemotePort       uint16    /* Remote port. */
	Protocol         uint8     /* Protocol. */
	_                [3]uint8
	_                uint32
}

type Flow struct {
	EndpointID       uint64    /* Endpoint ID. */
	ParentEndpointID uint64    /* Parent endpoint ID. */
	ProcessID        uint32    /* Process ID. */
	LocalAddress     [16]uint8 /* Local address. */
	RemoteAddress    [16]uint8 /* Remote address. */
	LocalPort        uint16    /* Local port. */
	RemotePort       uint16    /* Remote port. */
	Protocol         uint8     /* Protocol. */
	_                [3]uint8
	_                uint32
}

type Reflect struct {
	TimeStamp int64  /* Handle open time. */
	ProcessID uint32 /* Handle process ID. */
	layer     uint32 /* Handle layer. */
	Flags     uint64 /* Handle flags. */
	Priority  int16  /* Handle priority. */
	_         int16
	_         int32
	_         [4]uint64
}

func (r *Reflect) Layer() Layer {
	return Layer(r.layer)
}

type Address struct {
	Timestamp int64 /* Packet's timestamp. */
	layer     uint8 /* Packet's layer. */
	event     uint8 /* Packet event. */

	Flags uint8
	/* Packet was sniffed? */
	/* Packet is outbound? */
	/* Packet is loopback? */
	/* Packet is impostor? */
	/* Packet is IPv6? */
	/* Packet has valid IPv4 checksum? */
	/* Packet has valid TCP checksum? */
	/* Packet has valid UDP checksum? */

	_      uint8     /* Reserved1. */
	length uint32    /* Reserved2. */
	union  [64]uint8 /* Reserved3. */
	/* Network layer data. */
	/* Flow layer data. */
	/* Socket layer data. */
	/* Reflect layer data. */
}

func (a *Address) Layer() Layer {
	return Layer(a.layer)
}

func (a *Address) SetLayer(layer Layer) {
	a.layer = uint8(layer)
}

func (a *Address) Event() Event {
	return Event(a.event)
}

func (a *Address) SetEvent(event Event) {
	a.event = uint8(event)
}

func (a *Address) Sniffed() bool {
	return (a.Flags & uint8(0x01<<0)) == uint8(0x01<<0)
}

func (a *Address) SetSniffed() {
	a.Flags |= uint8(0x01 << 0)
}

func (a *Address) UnsetSniffed() {
	a.Flags &= ^uint8(0x01 << 0)
}

func (a *Address) Outbound() bool {
	return (a.Flags & uint8(0x01<<1)) == uint8(0x01<<1)
}

func (a *Address) SetOutbound() {
	a.Flags |= uint8(0x01 << 1)
}

func (a *Address) UnsetOutbound() {
	a.Flags &= ^uint8(0x01 << 1)
}

func (a *Address) Loopback() bool {
	return (a.Flags & uint8(0x01<<2)) == uint8(0x01<<2)
}

func (a *Address) SetLoopback() {
	a.Flags |= uint8(0x01 << 2)
}

func (a *Address) UnsetLoopback() {
	a.Flags &= ^uint8(0x01 << 2)
}

func (a *Address) Impostor() bool {
	return (a.Flags & uint8(0x01<<3)) == uint8(0x01<<3)
}

func (a *Address) SetImpostor() {
	a.Flags |= uint8(0x01 << 3)
}

func (a *Address) UnsetImpostor() {
	a.Flags &= ^uint8(0x01 << 3)
}

func (a *Address) IPv6() bool {
	return (a.Flags & uint8(0x01<<4)) == uint8(0x01<<4)
}

func (a *Address) SetIPv6() {
	a.Flags |= uint8(0x01 << 4)
}

func (a *Address) UnsetIPv6() {
	a.Flags &= ^uint8(0x01 << 4)
}

func (a *Address) IPChecksum() bool {
	return (a.Flags & uint8(0x01<<5)) == uint8(0x01<<5)
}

func (a *Address) SetIPChecksum() {
	a.Flags |= uint8(0x01 << 5)
}

func (a *Address) UnsetIPChecksum() {
	a.Flags &= ^uint8(0x01 << 5)
}

func (a *Address) TCPChecksum() bool {
	return (a.Flags & uint8(0x01<<6)) == uint8(0x01<<6)
}

func (a *Address) SetTCPChecksum() {
	a.Flags |= uint8(0x01 << 6)
}

func (a *Address) UnsetTCPChecksum() {
	a.Flags &= ^uint8(0x01 << 6)
}

func (a *Address) UDPChecksum() bool {
	return (a.Flags & uint8(0x01<<7)) == uint8(0x01<<7)
}

func (a *Address) SetUDPChecksum() {
	a.Flags |= uint8(0x01 << 7)
}

func (a *Address) UnsetUDPChecksum() {
	a.Flags &= ^uint8(0x01 << 7)
}

func (a *Address) Length() uint32 {
	return a.length >> 12
}

func (a *Address) SetLength(n uint32) {
	a.length = n << 12
}

func (a *Address) Network() *Network {
	return (*Network)(unsafe.Pointer(&a.union))
}

func (a *Address) Socket() *Socket {
	return (*Socket)(unsafe.Pointer(&a.union))
}

func (a *Address) Flow() *Flow {
	return (*Flow)(unsafe.Pointer(&a.union))
}

func (a *Address) Reflect() *Reflect {
	return (*Reflect)(unsafe.Pointer(&a.union))
}
