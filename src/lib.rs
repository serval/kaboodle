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
//! Kaboodle needs to be instantiated with a broadcast port number and an optional preferred network
//! interface for it to use
//! ```rust, no_run
//! use kaboodle::Kaboodle;
//! async fn example() {
//!     let preferred_interface = None;
//!     let mut kaboodle = Kaboodle::new(7475, preferred_interface).unwrap();
//!     kaboodle.start().await;
//!     let peers = kaboodle.peers().await;
//! }
//! ```

// todo
// - proper error handling
// - add a 'props' payload for peers to share info about themsleves
// - Infection-Style Dissemination: Instead of propagating node failure information via multicast, protocol messages are piggybacked on the ping messages used to determine node liveness. This is equivalent to gossip dissemination.
// - don't respond to join announcements 100% of the time; scale down as the size of the mesh grows to avoid overwhelming newcomers

use errors::KaboodleError;
use if_addrs::Interface;
use kaboodle::{generate_fingerprint, KaboodleInner};
use networking::{best_available_interface, create_broadcast_sockets};

use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};
use structs::{KnownPeers, PeerInfo, RunState};
use structs::{Peer, PeerState};
use tokio::net::UdpSocket;
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
}

impl Kaboodle {
    /// Create a new Kaboodle mesh client, broadcasting on the given port number.
    pub fn new(
        // UDP port number to use for multicast discovery of peers. All clients using a given port
        // number will discover and coordinate with each other; give your mesh a distinct UDP port
        // number that is not already well-known for another purpose.
        broadcast_port: u16,
        // Which network interface to use for communication; provide None here to have Kaboodle
        // select one automatically.
        preferred_interface: Option<Interface>,
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
        self.state = RunState::Running;

        // Set up our main communications socket
        let sock = {
            let ip = self.interface.ip();
            UdpSocket::bind(format!("{ip}:0")).await?
        };

        // Put our socket address into the known peers list
        let self_addr = sock.local_addr().unwrap();
        self.self_addr = Some(self_addr);
        self.known_peers.lock().await.insert(
            self_addr,
            PeerInfo {
                state: PeerState::Known(Instant::now()),
            },
        );

        // Set up our broadcast communications sockets
        let (broadcast_in_sock, broadcast_out_sock, broadcast_addr) =
            create_broadcast_sockets(&self.interface, &self.broadcast_port)?;

        let (cancellation_tx, cancellation_rx) = tokio::sync::mpsc::channel(1);
        self.cancellation_tx = Some(cancellation_tx);

        let mut inner = KaboodleInner {
            sock,
            self_addr,
            rng: ChaChaRng::from_entropy(),
            broadcast_addr,
            broadcast_in_sock,
            broadcast_out_sock,
            known_peers: self.known_peers.clone(),
            curious_peers: HashMap::new(),
            last_broadcast_time: None,
            cancellation_rx,
        };

        tokio::spawn(async move {
            inner.run().await;
        });

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
    pub async fn peers(&self) -> Vec<Peer> {
        let known_peers = self.known_peers.lock().await;
        known_peers.keys().copied().collect()
    }

    /// Get our current list of known peers and their current state.
    pub async fn peer_states(&self) -> KnownPeers {
        let known_peers = self.known_peers.lock().await;
        known_peers.clone()
    }
}
