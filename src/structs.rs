use serde_derive::{Deserialize, Serialize};
use std::{collections::HashMap, fmt::Display, net::SocketAddr, time::Instant};

pub type Peer = SocketAddr;
pub type KnownPeers = HashMap<Peer, PeerInfo>;

/// PeerInfo
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerInfo {
    pub state: PeerState,
}

/// PeerState represents our knowledge about the status of a given peer in the network at any given
/// moment in time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerState {
    /// Peer is known to us and believed to be up. We keep track of the last time we heard anything
    /// about this peer (e.g. received from them) so we can focus our per-tick pings on the most
    /// out-of-date peers.
    Known(Instant),

    /// We have sent a ping and are waiting for the response. We keep track of when we sent the ping
    /// and will send an indirect ping if a duration more than PING_TIMEOUT elapses.
    WaitingForPing(Instant),

    /// We have sent indirect ping requests to one or more other peers to see if any of them are
    /// able to communicate with this peer. We keep track of when we sent the ping request and will
    /// drop this peer if a duration more than PING_TIMEOUT elapses.
    WaitingForIndirectPing(Instant),
}

impl Display for PeerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            PeerState::Known(_) => "Known",
            PeerState::WaitingForPing(_) => "WaitingForPing",
            PeerState::WaitingForIndirectPing(_) => "WaitingForIndirectPing",
        };
        write!(f, "{str}")?;
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum RunState {
    NotStarted,
    Running,
    Stopped,
}

/// The SwimBroadcast enum represents messages that we broadcast to the entire mesh.
/// Section 4 of the SWIM paper mentions that failure information can be piggybacked into the non-
/// broadcast ping and ack messages and (section 4.1) disseminated using a gossip protocol, e.g.
/// adding failure info with a TTL that is decremented by each host before being passed along and
/// stopping when this TTL gets to 0. This would decrease our reliance on multicast support on the
/// local network and is something we should consider for the future, although we still require
/// multicast to work for initial peer discovery. A future iteration of this library may include the
/// ability to work without multicast at all, so long as an initial seed peer is given to us
/// somehow.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SwimBroadcast {
    /// Announces our existence to the mesh; Peer represents this instance of Kaboodle
    Join(Peer),
    /// Announces that we believe the given Peer (not us) is down
    Failed(Peer),
}

/// The SwimEnvelope structure wraps all messages that we will send directly to another peer. In
/// addition to containing a unique SwimMessage, it contains general information about the sender.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SwimEnvelope {
    /// The actual message that the peer is sending.
    pub msg: SwimMessage,
}

/// The SwimMessage enum represents messages we will send directly to another peer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SwimMessage {
    Ping,
    PingRequest(Peer),
    Ack {
        peer: Peer,
        mesh_fingerprint: u32,
        num_peers: u32,
    },
    KnownPeers(Vec<Peer>),
    KnownPeersRequest {
        mesh_fingerprint: u32,
        num_peers: u32,
    },
}
