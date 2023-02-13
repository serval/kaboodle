use serde_derive::{Deserialize, Serialize};
use std::{fmt::Display, net::SocketAddr, time::Instant};

pub type Peer = SocketAddr;

/// The SwimMessage enum represents any messages we will send to the mesh, whether those are direct
/// peer-to-peer messages or messages that are broadcast to the entire mesh. We should probably
/// bifurcate into distinct SwimMessage and SwimBroadcastMessage enums someday.
///
/// The majority of these have a Peer associated with them. This is for two reasons:
/// 1. Although we know the sender of any given message, broadcast messages always show the
///    broadcast address (IP address 0.0.0.0, with whatever the broadcast port number is), which
///    means that we can't directly tell who sent a broadcast message.
/// 2. Some of our operations (e.g. Ping) can be done indirectly -- we can ask a peer to ping
///    another peer, and their Ack needs to tell us who the ping acknowledgement is actually for.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SwimMessage {
    Join(Peer),
    Ping,
    PingRequest(Peer),
    Ack(Peer),
    Failed(Peer),
    KnownPeers(Vec<Peer>),
}

/// PeerState represents our knowledge about the status of a given peer in the network at any given
/// moment in time.
#[derive(Debug, Eq, PartialEq)]
pub enum PeerState {
    /// Peer is known to us and believed to be up.
    Known,

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
            PeerState::Known => "Known",
            PeerState::WaitingForPing(_) => "WaitingForPing",
            PeerState::WaitingForIndirectPing(_) => "WaitingForIndirectPing",
        };
        write!(f, "{str}")?;
        Ok(())
    }
}
