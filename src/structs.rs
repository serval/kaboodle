use bytes::Bytes;
use serde_derive::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::Display,
    net::SocketAddr,
    time::{Duration, Instant},
};

use crate::observable_hashmap::ObservableHashMap;

pub type Peer = SocketAddr;
pub type Fingerprint = u32;
pub type KnownPeers = ObservableHashMap<Peer, PeerInfo>;

/// PeerInfo
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerInfo {
    pub identity: Bytes,
    pub state: PeerState,
    pub latency: Option<Duration>,
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
    Join { addr: Peer, identity: Bytes },
    /// Announces that we believe the given Peer (not us) is down
    Failed(Peer),
    /// Sent to attempt to discover any member of the mesh, without the intent to join the mesh
    /// as a Peer.
    Probe(SocketAddr),
}

/// The SwimEnvelope structure wraps all messages that we will send directly to another peer. In
/// addition to containing a unique SwimMessage, it contains general information about the sender.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SwimEnvelope {
    /// Consumer-provided identity to uniquely identify this Kaboodle instance across sessions.
    pub identity: Bytes,
    /// The actual message that the peer is sending.
    pub msg: SwimMessage,
}

/// Sent in response to a SwimBroadcast::Probe message, this is used to tell the probe initiator the
/// identity of the responding node.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProbeResponse {
    pub identity: Bytes,
}

/// The SwimMessage enum represents messages we will send directly to another peer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SwimMessage {
    /// Sent to verify whether a peer is still participating in the met; should be replied to with
    /// an Ack.
    Ping,
    /// Sent request that that we ping the given peer on behalf of the sender, who is having trouble
    /// communicating with them directly and is trying to figure out whether the peer is down or if
    /// the network is just unreliable.
    PingRequest(Peer),
    /// Sent in response to a Ping or PingRequest.
    Ack {
        peer: Peer,
        mesh_fingerprint: u32,
        num_peers: u32,
    },
    /// Sent in response to a SwimBroadcast::Join or KnownPeersRequest message; this contains a map
    /// of all of the peers that we are confident in the state of.
    KnownPeers(HashMap<Peer, Bytes>),
    /// Sent to request a known peers map.
    KnownPeersRequest {
        mesh_fingerprint: u32,
        num_peers: u32,
    },
}
