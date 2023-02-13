// todo
// - proper error handling
// - add a 'props' payload for peers to share info about themsleves
// - Infection-Style Dissemination: Instead of propagating node failure information via multicast, protocol messages are piggybacked on the ping messages used to determine node liveness. This is equivalent to gossip dissemination.
// - Round-Robin Probe Target Selection: Instead of randomly picking a node to probe during each protocol time step, the protocol is modified so that each node performs a round-robin selection of probe target. This bounds the worst-case detection time of the protocol, without degrading the average detection time.

use std::{
    collections::HashMap,
    fmt::Display,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::{Duration, Instant},
};

use crate::networking::my_ipv4_addrs;

use rand::{rngs::ThreadRng, seq::SliceRandom, thread_rng};
use serde_derive::{Deserialize, Serialize};
use sha256::digest;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::{net::UdpSocket, time::sleep};

/// The minimum amount of time to wait between ticks; we keep track of how long it's been since the
/// start of the current tick and wait however long is required to keep the time between ticks as
/// close as possible to this duration.
const IDEAL_TICK_DURATION: Duration = Duration::from_millis(1000);

/// How many other peers to ask to ping an unresponsive peer on our behalf.
const NUM_INDIRECT_PING_PEERS: usize = 3;

/// How long to wait for an ack to a ping or indirect ping before assuming it's never going to come.
const PING_TIMEOUT: Duration = Duration::from_millis(2000);

/// How often to re-broadcast our Join message if we don't know about any other peers right now
const REBROADCAST_INTERVAL: Duration = Duration::from_millis(10000);

type Peer = SocketAddr;
#[derive(Serialize, Deserialize, Debug, Clone)]
enum SwimMessage {
    Join(Peer),
    Ping,
    PingRequest(Peer),
    Ack(Peer),
    Failed(Peer),
    KnownPeers(Vec<Peer>),
}

#[derive(Debug, Eq, PartialEq)]
pub enum PeerState {
    /// Peer is known to us and believed to be up.
    Known,

    /// We have sent a ping and are waiting for the response. We keep track of when we sent the ping
    /// and will send an indirect ping if more than PING_TIMEOUT elapse.
    WaitingForPing(Instant),

    /// We have sent indirect ping requests to one or more other peers to see if any of them are
    /// able to communicate with this peer. We keep track of when we sent the ping request and will
    /// drop this peer if more than PING_TIMEOUT elapses.
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

pub struct Gossip {
    rng: ThreadRng,
    sock: UdpSocket,
    broadcast_addr: SocketAddr,
    broadcast_sock: UdpSocket,
    self_addr: SocketAddr,
    known_peers: HashMap<Peer, PeerState>,
    curious_peers: HashMap<Peer, Vec<Peer>>,
    /// Keeps track of when we last broadcast a Join message
    last_broadcast_time: Option<Instant>,
}

impl Gossip {
    /// Creates a new Gossip mesh client, broadcasting on the given port number. All clients using
    /// a given port number will discover and coordinate with each other; give your mesh a distinct
    /// port number that is not already well-known for another purpose.
    pub async fn new(broadcast_port: u16) -> Gossip {
        // Set up our main communications socket
        let sock = {
            let ip = my_ipv4_addrs()[0];
            UdpSocket::bind(format!("{ip}:0"))
                .await
                .expect("Failed to bind")
        };
        let self_addr = sock.local_addr().unwrap();

        // Set up our broadcast communications socket

        let broadcast_addr =
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), broadcast_port));
        let broadcast_sock = {
            let broadcast_sock =
                Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
            broadcast_sock.set_broadcast(true).unwrap();
            broadcast_sock.set_nonblocking(true).unwrap();
            broadcast_sock.set_reuse_address(true).unwrap();
            broadcast_sock.set_reuse_port(true).unwrap();
            broadcast_sock
                .bind(&SockAddr::from(broadcast_addr))
                .expect("Failed to bind for broadcast");
            let broadcast_sock: std::net::UdpSocket = broadcast_sock.into();
            UdpSocket::from_std(broadcast_sock).unwrap()
        };

        // Maps from a peer's address to the known state of that peer. See PeerState for a description
        // of the individual states.
        let mut known_peers: HashMap<Peer, PeerState> = HashMap::new();
        known_peers.insert(self_addr, PeerState::Known);

        // Maps from a peer's address to a list of other peers who would like to be informed if we get
        // a ping ack from said peer.
        let curious_peers: HashMap<Peer, Vec<Peer>> = HashMap::new();

        Gossip {
            sock,
            self_addr,
            rng: thread_rng(),
            broadcast_addr,
            broadcast_sock,
            known_peers,
            curious_peers,
            last_broadcast_time: None,
        }
    }

    /// Returns an SHA-256 hash of the current list of peers. The list is sorted before hashing, so it
    /// should be stable against ordering differences across different hosts.
    pub fn get_fingerprint(&self) -> String {
        let mut hosts: Vec<String> = self
            .known_peers
            .keys()
            .map(|peer| peer.to_string())
            .collect();
        hosts.sort();
        digest(hosts.join(","))
    }

    /// Returns the address we use for one-to-one UDP messages.
    pub fn get_self_addr(&self) -> SocketAddr {
        self.self_addr
    }

    /// Returns our current list of known peers.
    pub fn get_peers(&self) -> &HashMap<Peer, PeerState> {
        &self.known_peers
    }

    /// Broadcasts the given message to the entire mesh.
    async fn broadcast_msg(&self, msg: &SwimMessage) {
        log::info!("BROADCAST {msg:?}");
        let out_bytes = bincode::serialize(&msg).expect("Failed to serialize");
        self.broadcast_sock
            .send_to(&out_bytes, self.broadcast_addr)
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

    /// Runs the next round of mesh maintainance. The logic here is based on the SWIM paper by
    /// Gupta et al:
    /// https://en.wikipedia.org/wiki/SWIM_Protocol
    /// http://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf
    pub async fn tick(&mut self) {
        let tick_start = Instant::now();

        let should_broadcast = if let Some(last_broadcast_time) = self.last_broadcast_time {
            // Re-broadcast if we only know about ourself and it has been a while since we tried
            self.known_peers.len() == 1
                && tick_start.duration_since(last_broadcast_time) >= REBROADCAST_INTERVAL
        } else {
            true
        };
        if should_broadcast {
            // Broadcast our existence
            self.last_broadcast_time = Some(tick_start);
            self.broadcast_msg(&SwimMessage::Join(self.self_addr)).await;
        }

        // Handle any broadcast messages
        // Note that, at least on macOS, if you run multiple copies of this app simultaneously, only
        // one instance will actually receive broadcast packets.
        loop {
            let mut buf = [0; 1024];
            let Ok((_len, sender)) = self.broadcast_sock.try_recv_from(&mut buf) else {
                // Nothing to receive
                break;
            };
            let Ok(msg) = bincode::deserialize::<SwimMessage>(&buf) else {
                log::warn!("Failed to deserialize bytes: {buf:?}");
                continue;
            };
            log::info!("RECV-BROADCAST [{sender}] {msg:?}");
            match msg {
                SwimMessage::Failed(peer) => {
                    if peer == self.self_addr {
                        // Someone else must've lost connectivity to us, but that doesn't mean we
                        // should forget about ourselves.
                        continue;
                    };

                    // Note: unclear whether we should unilaterally trust this but ok
                    log::info!("Removing peer that we were told has failed {peer}");
                    self.known_peers.remove(&peer);
                }
                SwimMessage::Join(peer) => {
                    if peer == self.self_addr {
                        continue;
                    }
                    log::info!("Got a join from {peer}");
                    self.known_peers.insert(peer, PeerState::Known);

                    // Send a list of known peers to the newcomer
                    // todo: we might need to only send a subset in order to keep packet size down;
                    // that is a problem for another day.
                    let other_peers: Vec<Peer> = self
                        .known_peers
                        .iter()
                        // todo: maybe exclude peers in the  WaitingForIndirectPing state, since
                        // they are suspect.
                        .filter(|(other_peer, _)| **other_peer != peer)
                        .map(|(other_peer, _)| *other_peer)
                        .collect();

                    if !other_peers.is_empty() {
                        self.send_msg(&peer, &SwimMessage::KnownPeers(other_peers))
                            .await;
                    }
                }
                _ => {
                    log::info!("Got unexpected broadcast {msg:?}");
                }
            }
        }

        // Handle any incoming messages
        loop {
            let mut buf = [0u8; 1024];
            let Ok((_len, sender)) = self.sock.try_recv_from(&mut buf) else {
                // No more messages for now
                break;
            };
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
                    self.known_peers.insert(peer, PeerState::Known);

                    if let Some(observers) = self.curious_peers.remove(&peer) {
                        // Some of our peers were waiting to hear back about this ping
                        for observer in observers {
                            // todo: run these in parallel
                            self.send_msg(&observer, &SwimMessage::Ack(peer)).await;
                        }
                    }
                }
                SwimMessage::KnownPeers(peers) => {
                    for peer in peers {
                        self.known_peers.entry(peer).or_insert(PeerState::Known);
                    }
                }
                SwimMessage::Ping => {
                    self.send_msg(&sender, &SwimMessage::Ack(self.self_addr))
                        .await;
                    self.known_peers.insert(sender, PeerState::Known);
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
                _ => {
                    log::info!("Received unexpected message: {msg:?}");
                }
            }
        }

        // Handle suspected peers
        // - for each suspected peer P,
        //   - if they have not responded to our ping request within PING_TIMEOUT_MS,
        //     - send pick N other random peers and send each of them a PING-REQ(P) message
        //     - each recipient sends their own PING to P and forwards any ACK to the requestor
        //     - if any peer ACKs, mark P as up
        //     - if no ACKs arrive within PING_TIMEOUT_MS,
        //       - remove it from the membership list
        //       - broadcast a FAILED(P) message to the mesh
        let mut removed_peers: Vec<Peer> = vec![];
        let mut indirectly_pinged_peers: Vec<Peer> = vec![];
        let non_suspected_peers = self
            .known_peers
            .iter()
            .filter(|(_, peer_state)| **peer_state == PeerState::Known)
            .map(|(peer, _)| *peer)
            .collect::<Vec<Peer>>();
        for (peer, peer_state) in self.known_peers.iter() {
            match peer_state {
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
                _ => {}
            }
        }
        for peer in indirectly_pinged_peers {
            self.known_peers
                .insert(peer, PeerState::WaitingForIndirectPing(Instant::now()));
        }
        for removed_peer in removed_peers {
            log::info!("Removing peer {removed_peer}");
            self.known_peers.remove(&removed_peer);
            self.curious_peers.remove_entry(&removed_peer);

            self.broadcast_msg(&SwimMessage::Failed(removed_peer)).await;
        }

        // Ping a random peer
        // - pick a known peer P at random
        let non_suspected_peers: Vec<Peer> = self
            .known_peers
            .iter()
            .filter(|(addr, peer_state)| {
                **peer_state == PeerState::Known && **addr != self.self_addr
            })
            .map(|(addr, _)| *addr)
            .collect();
        if let Some(target_peer) = non_suspected_peers.choose(&mut self.rng) {
            // - send a PING message to P and start a timeout
            //     - if P replies with an ACK, mark the peer as up
            self.known_peers
                .insert(*target_peer, PeerState::WaitingForPing(Instant::now()));

            // Comment out the following line to test indirect pinging
            self.send_msg(target_peer, &SwimMessage::Ping).await;
        }

        // Wait until the next tick
        let time_since_tick_start = Instant::now().duration_since(tick_start);
        let required_delay = IDEAL_TICK_DURATION - time_since_tick_start;
        if required_delay.as_millis() > 0 {
            sleep(required_delay).await;
        }
    }
}
