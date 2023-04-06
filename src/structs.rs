use bytes::{Bytes, BytesMut, Buf, BufMut};
use serde_derive::{Deserialize, Serialize};
use core::slice::SlicePattern;
use std::{collections::HashMap, convert, fmt::Display, net::SocketAddr, time::Instant};

use crate::observable_hashmap::ObservableHashMap;

pub type Peer = SocketAddr;
pub type Fingerprint = u32;
pub type KnownPeers = ObservableHashMap<Peer, PeerInfo>;

/// PeerInfo
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerInfo {
    pub identity: Bytes,
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

#[cfg(feature = "proximity")]
const PROXIMITY_DIMENSIONS: usize = 2;

/// A Coordinate type which aliases on a violin::Coord type if the proximity feature is used
/// and falls back to an empty type otherwise.
#[cfg(feature = "proximity")]
#[derive(Serialize, Deserialize, Debug, Clone)]
type Coordinate = violin::Coord<violin::VecD<PROXIMITY_DIMENSIONS>>;
#[cfg(not(feature = "proximity"))]
type Coordinate = ();

/// If the proximity feature is enabled, a CoordinateRecord contains a timestamp as well as
/// a Coordinate.
#[cfg(feature = "proximity")]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoordinateRecord {
    timestamp: Instant,
    coord: Coordinate,
}
#[cfg(not(feature = "proximity"))]
pub struct CoordinateRecord {}

/// If the proximity feature is enabled, Proximity is a data structure which holds a `violin::Node`
/// for our own node information, and a hash map of coordinate records for any peers which we have
/// received proximity information on.
#[cfg(feature = "proximity")]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Proximity {
    pub node: violin::Node<violin::VecD<PROXIMITY_DIMENSIONS>>,
    pub peers: HashMap<Peer, CoordinateRecord>,
}
#[cfg(not(feature = "proximity"))]
pub struct Proximity {}

#[cfg(feature = "proximity")]
impl Proximity {
    /// We are seeding the coordinates with some *very small* values derived from the identity
    /// such that every node starts out with essentially zero, but with a tiny amount of
    /// directionality. This helps spread out the coordinates in a consistent way for a given
    /// set of identities in a mesh.
    /// What we are doing in the following is derive a seed from the kaboodle identity and
    /// feed that into the (f64-based) coordinates in such a way that they result in very small
    /// numbers. See code for further implementation details.
    // Implementation note: This is a slightly hacky method derived from how double-precision floating-
    // point numbers are represented in memory (as per IEEE 754). There are some good explanations and
    // examples [on Wikipedia](https://en.wikipedia.org/wiki/Double-precision_floating-point_format),
    // briefly summarizing here:
    //   - the first bit of an f64 is its sign.
    //   - the subsequent 11 bits are the exponent in a 1023-offset notation, meaning 1023 is subtracted
    //     from this 11-bit number to determine the actual exponnent. This means that if the first bit of
    //     this 11-bit number is 1, the exponent will be positive, and it will be negative otherwise.
    //   - The remaining 52 bits are the so-called significand, represented as a binary fractional number.
    pub fn new(identity: Bytes) -> Proximity {
        // Hash the identity to get some quasi-random, but identity-consistent bytes
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&identity);
        // Throw the hash in a BytesMut structure twice to fill up 8 bytes
        let mut seed_input = BytesMut::with_capacity(8);
        seed_input.put_u32(hasher.finalize());
        seed_input.put_u32(hasher.finalize());
        // Enforce a length of 8, just to be sure
        seed_input.resize(8, 0x00);
        // Create a vector for our seed coordinates:
        let mut seeds: Vec<f64> = Vec::new();
        // Generate seed values for each of the `PROXIMITY_DIMENSIONS` dimensions:
        for _ in 0..PROXIMITY_DIMENSIONS {
            // rotate the input, otherwise all seeds will be on the unit vector.
            // This is a simple way to introduce some quasi-randomness which is directly derived from
            seed_input.rotate_right(1);
            // This is where it gets funky.
            // 1. We take whatever the current rotated value of `id_input` is.
            let mut sbytes = Bytes::from(seed_input);
            // 2. Then, we binary the first byte with the bitmap `0x1F`.
            //    What this does is ensure that the second and third bits will be zero. This is important
            //    because the second and third bits represent the most significant bits of a IEEE 754
            //    double-precision floating point value that we will eventually convert this into.
            //    If the second bit was 1, the exponent of this number would become positive, i.e. the
            //    quasi-random number we are seeding here would be very large.
            //    The third bit is the highest bit of the exponent -- we are zeroing it to ensure that
            //    whatever the other bits are set to, the resulting random number can never be high enough
            //    to cause real-world distortions. If the third bit were allowed to be 1, it could result
            //    in random seeds with coordinates up to around 2 -- of it is forced to zero, the seed
            //    values will be incredibly tiny, at a maximum of +1.4916681462400412e-154.
            //    This is small enough to basically start "at zero" but with a tiny bit of directionality
            //    for the algorithm to use initially.
            sbytes.as_mut()[0] = sbytes[0] & 0x9f;
            // We convert this raw big-endian representation of an IEEE 754 double-precision floating point
            // number to an f64 and push it to the seeds vector:
            seeds.push(f64::from_be_bytes(sbytes.to_vec().try_into().unwrap()));
            //seed.to_vec().try_into().unwrap()));
        }
        let seed_arr: [f64; PROXIMITY_DIMENSIONS] =
            Vec::<f64>::try_into(seeds).expect("Failed to generate a proximity seed");
        let seed_coords = violin::VecD::from(seed_arr);
        Proximity {
            node: violin::Node::with_coord(seed_coords),
            peers: HashMap::new(),
        }
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
    pub coord: Option<Coordinate>,
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
    PingRequest {
        peer: Peer,
        coord: Option<Coordinate>,
    },
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
