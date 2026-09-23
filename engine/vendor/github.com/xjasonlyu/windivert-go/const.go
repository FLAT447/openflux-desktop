package windivert

//WinDivert layers.
type Layer int

const (
	LayerNetwork        Layer = iota /* Network layer. */
	LayerNetworkForward              /* Network layer (forwarded packets) */
	LayerFlow                        /* Flow layer. */
	LayerSocket                      /* Socket layer. */
	LayerReflect                     /* Reflect layer. */
)

func (l Layer) String() string {
	switch l {
	case LayerNetwork:
		return "WINDIVERT_LAYER_NETWORK"
	case LayerNetworkForward:
		return "WINDIVERT_LAYER_NETWORK_FORWARD"
	case LayerFlow:
		return "WINDIVERT_LAYER_FLOW"
	case LayerSocket:
		return "WINDIVERT_LAYER_SOCKET"
	case LayerReflect:
		return "WINDIVERT_LAYER_REFLECT"
	default:
		return ""
	}
}

// WinDivert events.
type Event int

const (
	EventNetworkPacket   Event = iota /* Network packet. */
	EventFlowEstablished              /* Flow established. */
	EventFlowDeleted                  /* Flow deleted. */
	EventSocketBind                   /* Socket bind. */
	EventSocketConnect                /* Socket connect. */
	EventSocketListen                 /* Socket listen. */
	EventSocketAccept                 /* Socket accept. */
	EventSocketClose                  /* Socket close. */
	EventReflectOpen                  /* WinDivert handle opened. */
	EventReflectClose                 /* WinDivert handle closed. */
)

func (e Event) String() string {
	switch e {
	case EventNetworkPacket:
		return "WINDIVERT_EVENT_NETWORK_PACKET"
	case EventFlowEstablished:
		return "WINDIVERT_EVENT_FLOW_ESTABLISHED"
	case EventFlowDeleted:
		return "WINDIVERT_EVENT_FLOW_DELETED"
	case EventSocketBind:
		return "WINDIVERT_EVENT_SOCKET_BIND"
	case EventSocketConnect:
		return "WINDIVERT_EVENT_SOCKET_CONNECT"
	case EventSocketListen:
		return "WINDIVERT_EVENT_SOCKET_LISTEN"
	case EventSocketAccept:
		return "WINDIVERT_EVENT_SOCKET_ACCEPT"
	case EventSocketClose:
		return "WINDIVERT_EVENT_SOCKET_CLOSE"
	case EventReflectOpen:
		return "WINDIVERT_EVENT_REFLECT_OPEN"
	case EventReflectClose:
		return "WINDIVERT_EVENT_REFLECT_CLOSE"
	default:
		return ""
	}
}

// WinDivert shutdown parameter.
type Shutdown int

const (
	ShutdownRecv Shutdown = iota /* Shutdown recv. */
	ShutdownSend                 /* Shutdown send. */
	ShutdownBoth                 /* Shutdown recv and send. */
)

func (h Shutdown) String() string {
	switch h {
	case ShutdownRecv:
		return "WINDIVERT_SHUTDOWN_RECV"
	case ShutdownSend:
		return "WINDIVERT_SHUTDOWN_SEND"
	case ShutdownBoth:
		return "WINDIVERT_SHUTDOWN_BOTH"
	default:
		return ""
	}
}

// WinDivert parameters.
type Param int

const (
	QueueLength  Param = iota /* Packet queue length. */
	QueueTime                 /* Packet queue time. */
	QueueSize                 /* Packet queue size. */
	VersionMajor              /* Driver version (major). */
	VersionMinor              /* Driver version (minor). */
)

func (p Param) String() string {
	switch p {
	case QueueLength:
		return "WINDIVERT_PARAM_QUEUE_LENGTH"
	case QueueTime:
		return "WINDIVERT_PARAM_QUEUE_TIME"
	case QueueSize:
		return "WINDIVERT_PARAM_QUEUE_SIZE"
	case VersionMajor:
		return "WINDIVERT_PARAM_VERSION_MAJOR"
	case VersionMinor:
		return "WINDIVERT_PARAM_VERSION_MINOR"
	default:
		return ""
	}
}

// WinDivert flags.
const (
	FlagDefault   = 0x0000
	FlagSniff     = 0x0001
	FlagDrop      = 0x0002
	FlagRecvOnly  = 0x0004
	FlagReadOnly  = FlagRecvOnly
	FlagSendOnly  = 0x0008
	FlagWriteOnly = FlagSendOnly
	FlagNoInstall = 0x0010
	FlagFragments = 0x0020
)

// WinDivert constants.
const (
	PriorityDefault    = 0
	PriorityHighest    = 3000
	PriorityLowest     = -PriorityHighest
	QueueLengthDefault = 4096
	QueueLengthMin     = 32
	QueueLengthMax     = 16384
	QueueTimeDefault   = 2000     /* 2s */
	QueueTimeMin       = 100      /* 100ms */
	QueueTimeMax       = 16000    /* 16s */
	QueueSizeDefault   = 4194304  /* 4MB */
	QueueSizeMin       = 65535    /* 64KB */
	QueueSizeMax       = 33554432 /* 32MB */
	BatchMax           = 0xff     /* 255 */
	MTUMax             = 40 + 0xffff
)

const (
	ChecksumDefault  = 0
	NoIPChecksum     = 1
	NoICMPChecksum   = 2
	NoICMPV6Checksum = 4
	NoTCPChecksum    = 8
	NoUDPChecksum    = 16
)
