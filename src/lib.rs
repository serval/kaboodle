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
use discovery::discover_mesh_member;
use errors::KaboodleError;
use events::{handle_known_peers_events, DepartureSenders, DiscoverySenders, FingerprintSenders};
use if_addrs::Interface;
use kaboodle::{generate_fingerprint, KaboodleInner};
use networking::best_available_interface;
use observable_hashmap::ObservableHashMap;

use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use structs::{Fingerprint, KnownPeers, Peer, PeerInfo};
use tokio::sync::{
    mpsc::UnboundedReceiver,
    oneshot::Sender,
    oneshot::{channel, Receiver},
    Mutex,
};

mod discovery;
pub mod errors;
mod events;
mod kaboodle;
pub mod networking;
pub mod observable_hashmap;
mod structs;

/// Data managed by a Kaboodle mesh client.
#[derive(Debug)]
pub struct Kaboodle {
    known_peers: Arc<Mutex<KnownPeers>>,
    broadcast_port: u16,
    self_addr: Option<SocketAddr>,
    cancellation_tx: Option<Sender<()>>,
    discovery_tx: Arc<Mutex<DiscoverySenders>>,
    departure_tx: Arc<Mutex<DepartureSenders>>,
    fingerprint_tx: Arc<Mutex<FingerprintSenders>>,
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
        let Some(interface) = preferred_interface.or_else(|| best_available_interface().ok()) else {
            return Err(KaboodleError::NoAvailableInterfaces);
        };

        // Maps from a peer's address to the known state of that peer. See PeerState for a
        // description of the individual states.
        let known_peers = Arc::new(Mutex::new(ObservableHashMap::new()));

        // Spin up a background task to listen to change notifications coming from our known_peers
        // ObservableHashMap. It notifies us of additions, removals, and mutations; we derive more
        // semantic high-level notifications for our consumer based on those.
        let discovery_tx = Arc::new(Mutex::new(vec![]));
        let departure_tx = Arc::new(Mutex::new(vec![]));
        let fingerprint_tx = Arc::new(Mutex::new(vec![]));
        handle_known_peers_events(
            known_peers.clone(),
            discovery_tx.clone(),
            departure_tx.clone(),
            fingerprint_tx.clone(),
        );

        Ok(Kaboodle {
            known_peers,
            interface,
            broadcast_port,
            identity: identity.into(),
            discovery_tx,
            departure_tx,
            fingerprint_tx,

            // These will get set whenever `start` is called:
            self_addr: None,
            cancellation_tx: None,
        })
    }

    /// Tell the client to connect to the network and find other clients.
    pub async fn start(&mut self) -> Result<(), KaboodleError> {
        if self.self_addr.is_some() {
            return Ok(());
        }

        assert!(self.cancellation_tx.is_none());

        let result = KaboodleInner::start(
            &self.interface,
            self.broadcast_port,
            self.known_peers.clone(),
            self.identity.clone(),
        )
        .await?;

        self.self_addr = Some(result.self_addr);
        self.cancellation_tx = Some(result.cancellation_tx);

        Ok(())
    }

    /// Disconnect this client from the mesh network.
    pub async fn stop(&mut self) -> Result<(), KaboodleError> {
        if self.self_addr.is_none() {
            // we're not actually running ¯\_(ツ)_/¯
            return Ok(());
        }

        // Remove ourself from the known peers list and forget about our soon-to-be-previous
        // self_addr.
        if let Some(self_addr) = self.self_addr.take() {
            let mut known_peers = self.known_peers.lock().await;
            known_peers.remove(&self_addr);
        }

        let Some(cancellation_tx) = self.cancellation_tx.take() else {
            // This should not happen
            log::warn!("Unable to cancel daemon thread because we have no communication channel; this is a programming error.");
            return Err(KaboodleError::StoppingFailed);
        };

        if cancellation_tx.send(()).is_err() {
            return Err(KaboodleError::StoppingFailed);
        }

        Ok(())
    }

    /// Returns a channel receiver that will be invoked every time a peer leaves the mesh.
    pub fn discover_departures(&mut self) -> Result<UnboundedReceiver<SocketAddr>, KaboodleError> {
        // Create a channel and add it to our list of discovery channel (ha) transmitters. Whenever
        // Kaboodle is notified of a new peer by KaboodleInner, Kaboodle will send that peer out to
        // all of the transmitters.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let departure_tx = self.departure_tx.clone();
        tokio::spawn(async move {
            let mut locked = departure_tx.lock().await;
            locked.push(tx);
        });

        Ok(rx)
    }

    /// Returns a channel receiver that will be invoked every time the mesh fingerprint changes
    /// (e.g. when a peer arrives, leaves, or changes their identity payload). Note that the channel
    /// will not be passed the new fingerprint automatically; interested parties can retrieve the
    /// current (new) fingerprint by calling `self.fingerprint()`.
    pub fn discover_fingerprint_changes(
        &mut self,
    ) -> Result<UnboundedReceiver<Fingerprint>, KaboodleError> {
        // Create a channel and add it to our list of discovery channel (ha) transmitters. Whenever
        // Kaboodle is notified of a new peer by KaboodleInner, Kaboodle will send that peer out to
        // all of the transmitters.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let fingerprint_tx = self.fingerprint_tx.clone();
        tokio::spawn(async move {
            let mut locked = fingerprint_tx.lock().await;
            locked.push(tx);
        });

        Ok(rx)
    }

    /// Returns a channel receiver that will be invoked every time a new peer is discovered.
    pub fn discover_peers(
        &mut self,
    ) -> Result<UnboundedReceiver<(SocketAddr, Bytes)>, KaboodleError> {
        // Create a channel and add it to our list of discovery channel (ha) transmitters. Whenever
        // Kaboodle is notified of a new peer by KaboodleInner, Kaboodle will send that peer out to
        // all of the transmitters.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let discovery_tx = self.discovery_tx.clone();
        tokio::spawn(async move {
            let mut locked = discovery_tx.lock().await;
            locked.push(tx);
        });

        Ok(rx)
    }

    /// Returns a one-shot channel receiver that will be invoked the next time a new peer is
    /// discovered; use this if you just want to discover the very next peer.
    pub fn discover_next_peer(&mut self) -> Result<Receiver<(SocketAddr, Bytes)>, KaboodleError> {
        let (tx, rx) = channel::<(SocketAddr, Bytes)>();
        let mut discovery_rx = self.discover_peers()?;

        tokio::spawn(async move {
            if let Some(peer) = discovery_rx.recv().await {
                if !tx.is_closed() {
                    if let Err(err) = tx.send(peer) {
                        log::warn!(
                            "Failed to notify listener of newly discovered peer; err={err:?}"
                        );
                    }
                }
            }

            // `discovery_rx` will be dropped here, which will cause the tx end of that channel to
            // be closed. The next time a peer is discovered, that code will notice that this
            // channel is closed, and it will remove it from the list of channels to notify.
        });

        Ok(rx)
    }

    /// Calculate an CRC-32 hash of the current list of peers. The list is sorted before hashing, so
    /// it should be stable against ordering differences across different hosts.
    pub async fn fingerprint(&self) -> Fingerprint {
        let known_peers = self.known_peers.lock().await;
        generate_fingerprint(&known_peers)
    }

    /// Returns true if we are currently running (that is, if `.start()` has been called).
    pub fn is_running(&self) -> bool {
        self.self_addr.is_some()
    }

    /// Get the address we use for one-to-one UDP messages, if we are currently running.
    pub fn self_addr(&self) -> Option<SocketAddr> {
        self.self_addr
    }

    /// Get the network interface that we are communicating on.
    pub fn interface(&self) -> Interface {
        self.interface.clone()
    }

    /// Sets our identity payload to the given value. This can only be done while the mesh is not
    /// running.
    pub fn set_identity(&mut self, new_identity: Bytes) -> Result<(), KaboodleError> {
        if self.is_running() {
            // We can't update our identity value while we are a member of the mesh, because there
            // is no message for "hey, my identity changed". This is just a decision made to keep
            // things as simple as possible, and is one we can revisit later.
            return Err(KaboodleError::InvalidOperation(String::from(
                "Cannot change identity while the mesh is running; call .stop first",
            )));
        }

        self.identity = new_identity;

        Ok(())
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
    pub async fn peer_states(&self) -> HashMap<Peer, PeerInfo> {
        let known_peers = self.known_peers.lock().await;
        known_peers
            .iter()
            .map(|(peer, peer_info)| (peer.to_owned(), peer_info.to_owned()))
            .collect::<HashMap<Peer, PeerInfo>>()
    }

    /// Discovers one member of the mesh on the given port and interface, without actually joining
    /// the mesh. Use this if you simply need to discover a member of the mesh but do not want to
    /// join the mesh yourself.
    pub async fn discover_mesh_member(
        broadcast_port: u16,
        preferred_interface: Option<Interface>,
    ) -> Result<(SocketAddr, Bytes), KaboodleError> {
        let Some(interface) = preferred_interface.or_else(|| best_available_interface().ok()) else {
            return Err(KaboodleError::NoAvailableInterfaces);
        };
        let member = discover_mesh_member(broadcast_port, interface).await?;
        Ok(member)
    }
}
