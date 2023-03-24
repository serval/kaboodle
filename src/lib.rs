#![deny(future_incompatible, missing_docs, unsafe_code)]
#![warn(
    missing_debug_implementations,
    rust_2018_idioms,
    trivial_casts,
    unused_qualifications
)]
//! Kaboodle is a Rust crate containing an approximate implementation of the [SWIM membership gossip
//! protocol](http://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf), give or take
//! some details. It can be used to discover other peers on the LAN without any central
//! coordination.
//!
//! ```rust, no_run
//! use kaboodle::Kaboodle;
//! async fn example() {
//!     // UDP port number Kaboodle should use when discovering peers; every
//!     // Kaboodle instance must be using the same port number in order to find
//!     // each other.
//!     let port_number = 7475;
//!     // Which network interface to use; provide None to have Kaboodle select
//!     // one automatically.
//!     let preferred_interface = None;
//!     // Optional byte array used to durably identify this particular instance
//!     // of Kaboodle. Use this to give a particular machine a durable identity
//!     // if the application you are building on top of Kaboodle requires it.
//!     let identity = Some(b"instance1");
//!
//!     let mut kaboodle = Kaboodle::new(7475, preferred_interface, identity).unwrap();
//!     kaboodle.start().await;
//!     let peers = kaboodle.peers().await;
//!     for (peer_address, peer_identity) in peers {
//!        // do something interesting
//!     }
//! }
//! ```

use bytes::Bytes;
use errors::KaboodleError;
use if_addrs::Interface;
use kaboodle::{generate_fingerprint, KaboodleInner};
use networking::best_available_interface;

use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use structs::{KnownPeers, Peer, RunState};
use tokio::sync::{mpsc::Sender, Mutex};

pub mod errors;
mod kaboodle;
pub mod networking;
mod structs;

/// Data managed by a Kaboodle mesh client.
#[derive(Debug)]
pub struct Kaboodle {
    known_peers: Arc<Mutex<KnownPeers>>,
    state: RunState,
    broadcast_port: u16,
    self_addr: Option<SocketAddr>,
    cancellation_tx: Option<Sender<()>>,
    interface: Interface,
    identity: Bytes,
}

impl Kaboodle {
    /// Create a new Kaboodle mesh client.
    ///
    /// `broadcast_port` specifies the UDP port number to use for multicast discovery of peers. All
    /// clients using a given port number will discover and coordinate with each other; give your
    /// mesh a distinct UDP port number that is not already well-known for another purpose.
    ///
    /// `preferred_interface` allows you to specify the network interface to use for communication;
    /// provide None here to have Kaboodle select one automatically.
    ///
    /// `identity` is a blob of bytes used to uniquely identity a given instance of Kaboodle
    /// across multiple sessions. It isn't required, but if you need to durably identify a particular
    /// instance over time, you can use this field. It is treated as an opaque blob internally and
    /// can be used to store anything, but should be kept as small as possible since it is included
    /// in most of Kabdoodle's network transmissions.
    pub fn new(
        broadcast_port: u16,
        preferred_interface: Option<Interface>,
        identity: impl Into<Bytes>,
    ) -> Result<Kaboodle, KaboodleError> {
        // Maps from a peer's address to the known state of that peer. See PeerState for a
        // description of the individual states.
        let known_peers: KnownPeers = HashMap::new();

        let Some(interface) = preferred_interface.or_else(|| best_available_interface().ok()) else {
            return Err(KaboodleError::NoAvailableInterfaces);
        };

        Ok(Kaboodle {
            known_peers: Arc::new(Mutex::new(known_peers)),
            state: RunState::NotStarted,
            interface,
            broadcast_port,
            identity: identity.into(),

            // These will get set whenever `start` is called:
            self_addr: None,
            cancellation_tx: None,
        })
    }

    /// Tell the client to connect to the network and find other clients.
    pub async fn start(&mut self) -> Result<(), KaboodleError> {
        if self.state == RunState::Running {
            return Ok(());
        }
        assert!(self.cancellation_tx.is_none());
        assert!(self.self_addr.is_none());

        self.state = RunState::Running;
        let (self_addr, cancellation_tx) = KaboodleInner::start(
            &self.interface,
            self.broadcast_port,
            self.known_peers.clone(),
            self.identity.clone(),
        )
        .await?;

        self.self_addr = Some(self_addr);
        self.cancellation_tx = Some(cancellation_tx);

        Ok(())
    }

    /// Disconnect this client from the mesh network.
    pub async fn stop(&mut self) -> Result<(), KaboodleError> {
        if self.state != RunState::Running {
            return Ok(());
        }
        self.state = RunState::Stopped;

        // Remove ourself from the known peers list and forget about our soon-to-be-previous
        // self_addr.
        if let Some(self_addr) = self.self_addr.take() {
            let mut known_peers = self.known_peers.lock().await;
            known_peers.remove(&self_addr);
        }

        let Some(cancellation_tx) = self.cancellation_tx.take() else {
            // This should not happen
            log::warn!("Unable to cancel daemon thread because we have no communication channel; this is a programming error.");
            return Err(KaboodleError::StoppingFailed(String::from("No communication channel")));
        };

        if let Err(err) = cancellation_tx.send(()).await {
            return Err(KaboodleError::StoppingFailed(err.to_string()));
        }

        Ok(())
    }

    /// Calculate an CRC-32 hash of the current list of peers. The list is sorted before hashing, so
    /// it should be stable against ordering differences across different hosts.
    pub async fn fingerprint(&self) -> u32 {
        let known_peers = self.known_peers.lock().await;
        generate_fingerprint(&known_peers)
    }

    /// Get the address we use for one-to-one UDP messages, if we are currently running.
    pub fn self_addr(&self) -> Option<SocketAddr> {
        self.self_addr
    }

    /// Get our current list of known peers.
    pub async fn peers(&self) -> HashMap<Peer, Bytes> {
        let known_peers = self.known_peers.lock().await;
        known_peers
            .iter()
            .map(|(peer, peer_state)| (peer.to_owned(), peer_state.identity.to_owned()))
            .collect()
    }

    /// Get our current list of known peers and their current state.
    pub async fn peer_states(&self) -> KnownPeers {
        let known_peers = self.known_peers.lock().await;
        known_peers.clone()
    }
}
