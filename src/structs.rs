use serde_derive::{Deserialize, Serialize};
use std::{fmt::Display, net::SocketAddr, time::Instant};

pub type Peer = SocketAddr;

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
    Join(Peer),
    Failed(Peer),
}

/// The SwimMessage enum represents messages we will send directly to another peer.
///
/// The majority of these have a Peer associated with them. This is for two reasons:
/// 1. Although we know the sender of any given message, broadcast messages always show the
///    broadcast address (IP address 0.0.0.0, with whatever the broadcast port number is), which
///    means that we can't directly tell who sent a broadcast message.
/// 2. Some of our operations (e.g. Ping) can be done indirectly -- we can ask a peer to ping
///    another peer, and their Ack needs to tell us who the ping acknowledgement is actually for.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SwimMessage {
    Ping,
    PingRequest(Peer),
    Ack(Peer, String, u32),
    KnownPeers(Vec<Peer>),
    KnownPeersRequest(String, u32),
}
