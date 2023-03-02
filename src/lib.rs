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
use kaboodle::{generate_fingerprint, KaboodleInner, PayloadRequestResult};
use networking::best_available_interface;

use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use structs::{KnownPeers, Peer, RunState};
use tokio::{
    sync::{
        mpsc::{Sender, UnboundedSender},
        Mutex,
    },
    task::JoinHandle,
};

pub mod errors;
mod kaboodle;
pub mod networking;
mod structs;

type PayloadRequestListeners = HashMap<SocketAddr, Vec<Sender<PayloadRequestResult>>>;

/// Data managed by a Kaboodle mesh client.
#[derive(Debug)]
pub struct Kaboodle {
    known_peers: Arc<Mutex<KnownPeers>>,
    state: RunState,
    broadcast_port: u16,
    self_addr: Option<SocketAddr>,
    cancellation_tx: Option<Sender<()>>,
    payload_req_tx: Option<UnboundedSender<SocketAddr>>,
    pending_payload_requests: Arc<Mutex<PayloadRequestListeners>>,
    payload_receiver: Option<JoinHandle<()>>,
    interface: Interface,
    identity: Bytes,
    payload: Bytes,
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
    ///
    /// `payload` is a blob of bytes used for whatever purpose you like. Payloads are fetched from
    /// peers on-demand, so there is no overhead for using them, although they are still transmitted
    /// via UDP, so they shouldn't be excessively large. Payloads are appropriate for sharing small
    /// key/value stores of data about peers, e.g. port numbers for communicating with another
    /// service running on that node.
    pub fn new(
        broadcast_port: u16,
        preferred_interface: Option<Interface>,
        identity: impl Into<Bytes>,
        payload: impl Into<Bytes>,
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
            payload: payload.into(),
            pending_payload_requests: Arc::new(Mutex::new(HashMap::new())),

            // These will get set whenever `start` is called:
            self_addr: None,
            cancellation_tx: None,
            payload_req_tx: None,
            payload_receiver: None,
        })
    }

    /// Tell the client to connect to the network and find other clients.
    pub async fn start(&mut self) -> Result<(), KaboodleError> {
        if self.state == RunState::Running {
            return Ok(());
        }
        assert!(self.cancellation_tx.is_none());
        assert!(self.payload_req_tx.is_none());
        assert!(self.self_addr.is_none());

        self.state = RunState::Running;
        let (self_addr, cancellation_tx, payload_req_tx, mut payload_recv_rx) =
            KaboodleInner::start(
                &self.interface,
                self.broadcast_port,
                self.known_peers.clone(),
                self.identity.clone(),
                self.payload.clone(),
            )
            .await?;

        self.self_addr = Some(self_addr);
        self.cancellation_tx = Some(cancellation_tx);
        self.payload_req_tx = Some(payload_req_tx);

        let pending_payload_requests = self.pending_payload_requests.clone();
        self.payload_receiver = Some(tokio::spawn(async move {
            while let Some((peer, res)) = payload_recv_rx.recv().await {
                let mut pending_payload_requests = pending_payload_requests.lock().await;
                let Some(listeners) = pending_payload_requests.remove(&peer) else {
                    match res.err() {
                        Some(KaboodleError::FailedPeerPayloadUnavailable) => {
                            // this isn't actually an error case; KaboodleInner sends this whenever
                            // a peer fails, regardless of whether there are listeners, because it
                            // doesn't keep track of listeners -- we do.
                        },
                        _ => log::info!("No listeners are waiting for a payload from peer {peer}; this is a programming error"),
                    }
                    continue;
                };
                drop(pending_payload_requests);
                for tx in listeners {
                    if !tx.is_closed() {
                        if let Err(err) = tx.send(res.clone()).await {
                            log::warn!("Failed to notify listener of payload: {err:?}");
                        }
                    }
                }
            }
            panic!("Payload receiver channel closed unexpectedly");
        }));

        Ok(())
    }

    /// Disconnect this client from the mesh network.
    pub async fn stop(&mut self) -> Result<(), KaboodleError> {
        if self.state != RunState::Running {
            return Ok(());
        }
        self.state = RunState::Stopped;

        // Stop the task that is listening for incoming payloads from KaboodleInner
        if let Some(payload_receiver) = self.payload_receiver.take() {
            payload_receiver.abort();
        }
        // Notify all pending payload listeners that their payload will never arrive
        let mut pending_payload_requests = self.pending_payload_requests.lock().await;
        for (_, listeners) in pending_payload_requests.drain() {
            for listener in listeners {
                let _ = listener.send(Err(KaboodleError::PayloadsUnavailableNotRunning));
            }
        }

        // Remove ourself from the known peers list and forget about our soon-to-be-previous
        // self_addr.
        if let Some(self_addr) = self.self_addr.take() {
            let mut known_peers = self.known_peers.lock().await;
            known_peers.remove(&self_addr);
        }

        // Close our payload channels
        self.payload_req_tx = None;

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

    /// Fetches the payload from the given peer.
    pub async fn peer_payload(&mut self, peer: SocketAddr) -> PayloadRequestResult {
        let Some(payload_req_tx) = self.payload_req_tx.as_ref() else {
            return Err(KaboodleError::PayloadsUnavailableNotRunning)
        };

        // There are a few moving parts involved in fetching the payload from a peer:
        // There's a KaboodleInner instance running the mesh. When we started that instance up, it
        // gave us a channel `req_payload_tx` to use whenever we'd like to request a peer's payload.
        // We send a SocketAddr over that channel, and it sends a SwimMessage::PayloadRequest
        // message to the peer at that address (note: it does not ensure that we actually have a
        // known peer at the given address).
        // Whenever KaboodleInner gets a SwimMessage::PayloadResponse from a peer, it will send that
        // payload back to us over the `recv_payload_rx` signal that KaboodleInner::start gave us.
        // In our `start` method, we spun up a background task that watches the `recv_payload_rx`
        // channel and notifies any listeners who are interested in the given peer that its payload
        // has arrived.
        // So, our job here is to add ourselves as a listener for that background task to notify,
        // and then to send the SocketAddr of the peer we're interested in over `req_payload_tx`.

        let (tx, mut rx) = tokio::sync::mpsc::channel::<PayloadRequestResult>(1);
        let mut pending_payload_requests = self.pending_payload_requests.lock().await;
        if let Some(listeners) = pending_payload_requests.get_mut(&peer) {
            // Someone else has already requested the payload from this peer, so just add our `tx`
            // channel to the list of listeners.
            listeners.push(tx);
        } else {
            // No one else has requested the payload from this peer yet, so create a list of
            // listeners for that peer containing our `tx` channel and send the peer's SocketAddr to
            // the `payload_req_tx` channel that's attached to KaboodleInner so it actually sends
            // a payload request to the peer.
            pending_payload_requests.insert(peer, vec![tx]);
            payload_req_tx.send(peer)?;
        }
        drop(pending_payload_requests);

        // Whenever the payload comes in, the background task that's communicating with
        // KaboodleInner will send it to our `tx` channel. Wait for it to arrive here; note that
        // we don't currently have any support for timeouts, although if the peer goes away, the
        // channel will be closed and we'll get an error to propagate here.
        if let Some(res) = rx.recv().await {
            return res;
        }

        // Note: this shouldn't ever actually happen, because:
        //  - if the peer we're waiting for has failed, we will receive a
        //    FailedPeerPayloadUnavailable message
        // - if we get .stop()'d, we will receive a PayloadsUnavailableNotRunning message
        // There shouldn't be any other cases that would lead to our channel getting closed.
        Err(KaboodleError::ChannelClosed)
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
