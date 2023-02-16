#![forbid(unsafe_code)]
#![deny(future_incompatible, missing_docs)]
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
//! Kaboodle needs to be instantiated with a broadcast port number:
//! ```rust
//! let kaboodle = Kaboodle::new(7475);
//! kaboodle.start().await;
//! let peers = kaboodle.peers().await;
//! ```

// todo
// - proper error handling
// - add a 'props' payload for peers to share info about themsleves
// - Infection-Style Dissemination: Instead of propagating node failure information via multicast, protocol messages are piggybacked on the ping messages used to determine node liveness. This is equivalent to gossip dissemination.
// - don't respond to join announcements 100% of the time; scale down as the size of the mesh grows to avoid overwhelming newcomers

use rand::{seq::SliceRandom, Rng};
use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
use sha256::digest;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::{Duration, Instant},
};
use structs::{Peer, PeerState, RunState, SwimBroadcast, SwimMessage};
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{Receiver, Sender},
        Mutex,
    },
    time::sleep,
};

mod structs;

mod networking;
use crate::networking::my_ipv4_addrs;

/// The minimum amount of time to wait between rounds of communication with the mesh; we keep track
/// of how long it's been since the start of the current tick and wait however long is required to
/// keep the time between ticks as close as possible to this duration. This could become a tuneable
/// parameter in the future, but is left as a constant for the sake of simplicity for now.
/// The SWIM paper notes that the protocol period must be at least three times longer than the
/// estimated round-trip time within the network, but that the protocol period they use in practice
/// is much longer than that.
const PROTOCOL_PERIOD: Duration = Duration::from_millis(1000);

/// How large a buffer to use when reading from our sockets; messages larger than this will be
/// truncated.
// TODO: Figure out what an actually optimal size would be.
const INCOMING_BUFFER_SIZE: usize = 1024;

/// How many other peers to ask to ping an unresponsive peer on our behalf.
const NUM_INDIRECT_PING_PEERS: usize = 3;

/// How long to wait for an ack to a ping or indirect ping before assuming it's never going to come.
/// Per the SWIM paper, this could be based on an estimate of the distribution of round-trip
/// time in the network, e.g. an average or the 99th percentile.
const PING_TIMEOUT: Duration = Duration::from_millis(2000);

/// How often to re-broadcast our Join message if we don't know about any other peers right now.
const REBROADCAST_INTERVAL: Duration = Duration::from_millis(10000);

/// Data managed by a Kaboodle mesh client.
#[derive(Debug)]
pub struct Kaboodle {
    known_peers: Arc<Mutex<HashMap<Peer, PeerState>>>,
    state: RunState,
    broadcast_port: u16,
    self_addr: Option<SocketAddr>,
    cancellation_tx: Option<Sender<()>>,
}

impl Kaboodle {
    /// Create a new Kaboodle mesh client, broadcasting on the given port number. All clients using
    /// a given port number will discover and coordinate with each other; give your mesh a distinct
    /// port number that is not already well-known for another purpose.
    pub fn new(broadcast_port: u16) -> Kaboodle {
        // Maps from a peer's address to the known state of that peer. See PeerState for a
        // description of the individual states.
        let known_peers: HashMap<Peer, PeerState> = HashMap::new();

        Kaboodle {
            known_peers: Arc::new(Mutex::new(known_peers)),
            state: RunState::NotStarted,
            broadcast_port,
            self_addr: None,
            cancellation_tx: None,
        }
    }

    /// Tell the client to connect to the network and find other clients.
    pub async fn start(&mut self) {
        if self.state == RunState::Running {
            return;
        }
        self.state = RunState::Running;

        // Set up our main communications socket
        let sock = {
            let ip = my_ipv4_addrs()[0];
            UdpSocket::bind(format!("{ip}:0"))
                .await
                .expect("Failed to bind")
        };

        // Put our socket address into the known peers list
        let self_addr = sock.local_addr().unwrap();
        self.self_addr = Some(self_addr);
        self.known_peers
            .lock()
            .await
            .insert(self_addr, PeerState::Known(Instant::now()));

        // Set up our broadcast communications socket
        // For listening to broadcasts, we need to bind our socket to 0.0.0.0:<port>, but for
        // sending broadcasts, we need to send to 255.255.255.255:<port>.
        let broadcast_inbound_addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(0, 0, 0, 0),
            self.broadcast_port,
        ));
        let broadcast_outbound_addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(255, 255, 255, 255),
            self.broadcast_port,
        ));
        let broadcast_sock = {
            let broadcast_sock =
                Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            broadcast_sock.set_broadcast(true).unwrap();
            broadcast_sock.set_nonblocking(true).unwrap();
            broadcast_sock.set_reuse_address(true).unwrap();
            broadcast_sock.set_reuse_port(true).unwrap();
            broadcast_sock
                .bind(&SockAddr::from(broadcast_inbound_addr))
                .expect("Failed to bind for broadcast");
            let broadcast_sock: std::net::UdpSocket = broadcast_sock.into();
            UdpSocket::from_std(broadcast_sock).unwrap()
        };

        let (cancellation_tx, cancellation_rx) = tokio::sync::mpsc::channel(1);
        self.cancellation_tx = Some(cancellation_tx);

        let mut inner = KaboodleInner {
            sock,
            self_addr,
            rng: ChaChaRng::from_entropy(),
            broadcast_outbound_addr,
            broadcast_sock,
            known_peers: self.known_peers.clone(),
            curious_peers: HashMap::new(),
            last_broadcast_time: None,
            cancellation_rx,
        };

        tokio::spawn(async move {
            inner.run().await;
        });
    }

    /// Disconnect this client from the mesh network.
    pub async fn stop(&mut self) {
        if self.state != RunState::Running {
            return;
        }
        self.state = RunState::Stopped;

        let Some(cancellation_tx) = self.cancellation_tx.take() else {
            // This should not happen
            log::warn!("Unable to cancel daemon thread because we have no communication channel; this is a programming error.");
            return;
        };

        cancellation_tx
            .send(())
            .await
            .expect("Failed to send cancellation request to daemon thread");
    }

    /// Calculate an SHA-256 hash of the current list of peers. The list is sorted before hashing, so it
    /// should be stable against ordering differences across different hosts.
    pub async fn fingerprint(&self) -> String {
        let known_peers = self.known_peers.lock().await;
        let mut hosts: Vec<String> = known_peers.keys().map(|peer| peer.to_string()).collect();
        hosts.sort();
        digest(hosts.join(","))
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
    pub async fn peer_states(&self) -> HashMap<Peer, PeerState> {
        let known_peers = self.known_peers.lock().await;
        known_peers.clone()
    }
}

struct KaboodleInner {
    rng: ChaChaRng,
    sock: UdpSocket,
    broadcast_outbound_addr: SocketAddr,
    broadcast_sock: UdpSocket,
    self_addr: SocketAddr,
    known_peers: Arc<Mutex<HashMap<Peer, PeerState>>>,
    // Maps from a peer's address to a list of other peers who would like to be informed if we get
    // a ping ack from said peer.
    curious_peers: HashMap<Peer, Vec<Peer>>,
    /// Keeps track of when we last broadcast a Join message
    last_broadcast_time: Option<Instant>,
    // Whether we should stop running; takes effect in the next tick
    cancellation_rx: Receiver<()>,
}

impl KaboodleInner {
    /// Broadcasts the given message to the entire mesh.
    async fn broadcast_msg(&self, msg: &SwimBroadcast) {
        log::info!("BROADCAST {msg:?}");
        let out_bytes = bincode::serialize(&msg).expect("Failed to serialize");
        self.broadcast_sock
            .send_to(&out_bytes, self.broadcast_outbound_addr)
            .await
            .expect("Failed to send");
    }

    /// Sends the given mesh to a single specific peer.
    async fn send_msg(&self, target_peer: &SocketAddr, msg: &SwimMessage) {
        log::info!("SEND [{target_peer}] {msg:?}");
        let out_bytes = bincode::serialize(&msg).expect("Failed to serialize");
        self.sock
            .send_to(&out_bytes, target_peer)
            .await
            .expect("Failed to send");
    }

    async fn maybe_broadcast_join(&mut self) {
        let now = Instant::now();
        let should_broadcast = if let Some(last_broadcast_time) = self.last_broadcast_time {
            // Re-broadcast if we only know about ourself and it has been a while since we tried
            let known_peers = self.known_peers.lock().await;
            known_peers.len() == 1
                && now.duration_since(last_broadcast_time) >= REBROADCAST_INTERVAL
        } else {
            true
        };
        if should_broadcast {
            // Broadcast our existence
            self.last_broadcast_time = Some(now);
            self.broadcast_msg(&SwimBroadcast::Join(self.self_addr))
                .await;
        }
    }

    /// Handle any incoming broadcast messages
    /// Note that, at least on macOS, if you run multiple copies of this app simultaneously, only
    /// one instance will actually receive broadcast packets.
    async fn handle_incoming_broadcasts(&mut self) {
        let mut buf = [0; INCOMING_BUFFER_SIZE];
        while let Ok((_len, sender)) = self.broadcast_sock.try_recv_from(&mut buf) {
            let Ok(msg) = bincode::deserialize::<SwimBroadcast>(&buf) else {
                log::warn!("Failed to deserialize bytes: {buf:?}");
                continue;
            };
            log::info!("RECV-BROADCAST [{sender}] {msg:?}");
            match msg {
                SwimBroadcast::Failed(peer) => {
                    if peer == self.self_addr {
                        // Someone else must've lost connectivity to us, but that doesn't mean we
                        // should forget about ourselves.
                        continue;
                    };

                    // Note: unclear whether we should unilaterally trust this but ok
                    log::info!("Removing peer that we were told has failed {peer}");
                    let mut known_peers = self.known_peers.lock().await;
                    known_peers.remove(&peer);
                    drop(known_peers);
                }
                SwimBroadcast::Join(peer) => {
                    if peer == self.self_addr {
                        continue;
                    }
                    log::info!("Got a join from {peer}");
                    let mut known_peers = self.known_peers.lock().await;
                    known_peers.insert(peer, PeerState::Known(Instant::now()));

                    // Figure out whether we should send the new peer our list of known peers
                    // Everyone in the mesh receives these broadcasts, so we don't want to respond
                    // 100% of the time -- otherwise, new peers in large meshes would receive an
                    // avalanche of redundant peer lists from every other peer when they first show
                    // up. The exact formula may get tweaked over time, but the intention is to
                    // ramp down from a 100% chance if we don't know of any other peers yet to a
                    // minimum of 1% once we have some sufficient number.
                    // See Section 3.2 in the SWIM paper for more thoughts on this logic.
                    // TODO: temporarily commented out because this logic makes large numbers of
                    // peers more likely to accidentally end up in several sub-meshes, rather than
                    // one large mesh. More thought required.
                    // let num_other_peers = known_peers.len() - 2; // minus ourselves and the newcomer
                    // let percent_chance_of_sending_peers =
                    //     std::cmp::max(1, 100 - usize::pow(num_other_peers, 2)) as f32 / 100.0;
                    // let should_send_peers = self.rng.gen::<f32>() < percent_chance_of_sending_peers;
                    // if !should_send_peers {
                    //     log::info!("Not sending known peers to new peer");
                    //     drop(known_peers);
                    //     continue;
                    // }

                    // Send a list of known peers to the newcomer
                    // todo: we might need to only send a subset in order to keep packet size down;
                    // that is a problem for another day.
                    let other_peers: Vec<Peer> = known_peers
                        .iter()
                        // todo: maybe exclude peers in the  WaitingForIndirectPing state, since
                        // they are suspect.
                        .filter(|(other_peer, _)| **other_peer != peer)
                        .map(|(other_peer, _)| *other_peer)
                        .collect();
                    drop(known_peers);

                    if !other_peers.is_empty() {
                        self.send_msg(&peer, &SwimMessage::KnownPeers(other_peers))
                            .await;
                    }
                }
            }
        }
    }

    async fn handle_incoming_messages(&mut self) {
        let mut buf = [0; INCOMING_BUFFER_SIZE];
        while let Ok((_len, sender)) = self.sock.try_recv_from(&mut buf) {
            let Ok(msg) = bincode::deserialize::<SwimMessage>(&buf) else {
                log::warn!("Failed to deserialize bytes: {buf:?}");
                continue;
            };
            log::info!("RECV [{sender}] {msg:?}");

            match msg {
                SwimMessage::Ack(peer) => {
                    // Insert the peer into our known_peers map with None as their "suspected since"
                    // timestamp; if they were already in there with a suspected since timestamp,
                    // this will reset them back to being non-suspected.
                    let mut known_peers = self.known_peers.lock().await;
                    known_peers.insert(peer, PeerState::Known(Instant::now()));
                    drop(known_peers);

                    if let Some(observers) = self.curious_peers.remove(&peer) {
                        // Some of our peers were waiting to hear back about this ping
                        for observer in observers {
                            // todo: run these in parallel
                            self.send_msg(&observer, &SwimMessage::Ack(peer)).await;
                        }
                    }
                }
                SwimMessage::KnownPeers(peers) => {
                    let mut known_peers = self.known_peers.lock().await;
                    let now = Instant::now();
                    for peer in peers {
                        known_peers.entry(peer).or_insert(PeerState::Known(now));
                    }
                    drop(known_peers);
                }
                SwimMessage::Ping => {
                    self.send_msg(&sender, &SwimMessage::Ack(self.self_addr))
                        .await;
                    let mut known_peers = self.known_peers.lock().await;
                    known_peers.insert(sender, PeerState::Known(Instant::now()));
                    drop(known_peers);
                }
                SwimMessage::PingRequest(peer) => {
                    // Make a note of the fact that `sender` wants to hear whenever we get an ack
                    // back from `peer`.
                    let mut observers = self.curious_peers.remove(&peer).unwrap_or_default();
                    if !observers.contains(&sender) {
                        observers.push(sender);
                    }
                    self.curious_peers.insert(peer, observers);

                    self.send_msg(&peer, &SwimMessage::Ping).await;
                }
            }
        }
    }

    async fn handle_suspected_peers(&mut self) {
        // Handle suspected peers
        // - for each suspected peer P,
        //   - if they have not responded to our ping request within PING_TIMEOUT_MS,
        //     - pick N other random peers and send each of them a PING-REQ(P) message
        //     - each recipient sends their own PING to P and forwards any ACK to the requestor
        //     - if any peer ACKs, mark P as up
        //     - if no ACKs arrive within PING_TIMEOUT_MS,
        //       - remove it from the membership list
        //       - broadcast a FAILED(P) message to the mesh
        let mut removed_peers: Vec<Peer> = vec![];
        let mut indirectly_pinged_peers: Vec<Peer> = vec![];
        let mut known_peers = self.known_peers.lock().await;
        let non_suspected_peers = known_peers
            .iter()
            .filter(|(peer, peer_state)| {
                **peer != self.self_addr && matches!(peer_state, PeerState::Known(_))
            })
            .map(|(peer, _)| *peer)
            .collect::<Vec<Peer>>();

        // Note: iterating over every known peer to find the ones in the WaitingFor... states is
        // obviously inefficient. If that turns out to be a problem, we can create separate lists
        // for those peers. Until it's known to be a problem, however, the simplicity of just having
        // a single HashMap remains appealling.
        for (peer, peer_state) in known_peers.iter() {
            match peer_state {
                PeerState::Known(_) => {
                    // Nothing required
                }
                PeerState::WaitingForPing(ping_sent) => {
                    if Instant::now().duration_since(*ping_sent) < PING_TIMEOUT {
                        continue;
                    };

                    let indirect_ping_peers: Vec<&Peer> = non_suspected_peers
                        .choose_multiple(&mut self.rng, NUM_INDIRECT_PING_PEERS)
                        .collect();

                    if indirect_ping_peers.is_empty() {
                        log::info!("No indirect peers to ask to ping {peer}; removing them now");
                        // There's no one we can ask for an indirect ping, so give up on this peer
                        // right away I guess.
                        removed_peers.push(*peer);
                        continue;
                    }

                    // Ask these peers to ping the suspected peer on our behalf
                    indirectly_pinged_peers.push(*peer);
                    let msg = SwimMessage::PingRequest(*peer);
                    for indirect_ping_peer in indirect_ping_peers {
                        // todo: run in parallel
                        self.send_msg(indirect_ping_peer, &msg).await;
                    }
                }
                PeerState::WaitingForIndirectPing(ping_sent) => {
                    if Instant::now().duration_since(*ping_sent) < PING_TIMEOUT {
                        continue;
                    };

                    // Give up on 'em
                    log::info!(
                        "Suspected peer {peer} timed out and is presumed down; removing them"
                    );
                    removed_peers.push(*peer);
                }
            }
        }

        for peer in indirectly_pinged_peers {
            known_peers.insert(peer, PeerState::WaitingForIndirectPing(Instant::now()));
        }

        for removed_peer in removed_peers {
            log::info!("Removing peer {removed_peer}");
            known_peers.remove(&removed_peer);
            self.curious_peers.remove_entry(&removed_peer);

            self.broadcast_msg(&SwimBroadcast::Failed(removed_peer))
                .await;
        }
    }

    async fn ping_random_peer(&mut self) {
        // The original SWIM implementation chose a peer to ping at random, but detailed an
        // improvement for round-robin target peer selection in section 4.3. Choosing a target
        // deterministically gives us a time bounded completeness guarantee: the time interval
        // between a peer becoming unavailable and the mesh detecting it is no more than 2 * N
        // ticks, where N is the total number of peers.
        let mut known_peers = self.known_peers.lock().await;
        let mut non_suspected_peers = known_peers
            .iter()
            .filter(|(peer, peer_state)| {
                **peer != self.self_addr && matches!(peer_state, PeerState::Known(_))
            })
            .map(|(peer, peer_state)| match peer_state {
                PeerState::Known(last_pinged) => (*peer, *last_pinged),
                _ => panic!("This code should never execute"),
            })
            .collect::<Vec<(Peer, Instant)>>();

        // Find the peer with the oldest "Known" timestamp; this is the one that our knowledge of
        // is most out of date.
        non_suspected_peers.sort_by_key(|(_, last_pinged)| *last_pinged);
        let Some((target_peer, _)) = non_suspected_peers.first() else {
            // No one to ping
            return;
        };

        // - send a PING message to P and start a timeout
        //     - if P replies with an ACK, mark the peer as up
        known_peers.insert(*target_peer, PeerState::WaitingForPing(Instant::now()));
        drop(known_peers);

        // Comment out the following line to test indirect pinging
        self.send_msg(target_peer, &SwimMessage::Ping).await;
    }

    /// Runs the next round of mesh maintainance. The logic here is based on the SWIM paper by
    /// Gupta et al:
    /// https://en.wikipedia.org/wiki/SWIM_Protocol
    /// http://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf
    async fn tick(&mut self) {
        // Building and maintaining the mesh consists of a number of subtasks that are repeated in
        // each tick.
        // In theory, we can run all of these things in parallel. This is left as an exercise for
        // the future; for now, it's nice to be able to reason about the logic of the mesh as a
        // series of individual steps happening over and over.
        self.maybe_broadcast_join().await;
        self.handle_incoming_broadcasts().await;
        self.handle_incoming_messages().await;
        self.handle_suspected_peers().await;
        self.ping_random_peer().await;
    }

    async fn run(&mut self) {
        // Run until we receive an event on our cancellation channel
        while self.cancellation_rx.try_recv().is_err() {
            let tick_start = Instant::now();

            self.tick().await;

            // Wait until the next tick
            let time_since_tick_start = Instant::now().duration_since(tick_start);
            let required_delay = PROTOCOL_PERIOD - time_since_tick_start;
            if required_delay.as_millis() > 0 {
                sleep(required_delay).await;
            }
        }
    }
}
