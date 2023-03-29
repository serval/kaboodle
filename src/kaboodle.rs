//! This module contains the core implementation of the SWIM gossip protocol.

use crate::errors::KaboodleError;
use crate::networking::create_broadcast_sockets;
use crate::structs::{
    KnownPeers, Peer, PeerInfo, PeerState, SwimBroadcast, SwimEnvelope, SwimMessage,
};
use bytes::Bytes;
use if_addrs::Interface;
use rand::SeedableRng;
use rand::{
    seq::{IteratorRandom, SliceRandom},
    Rng,
};
use rand_chacha::ChaChaRng;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc::Receiver, mpsc::Sender, Mutex},
    time::sleep,
};

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

/// How recently a peer must have been put into the Known state for us to include them in the list
/// of peers we send in response to KnownPeersRequest messages.
/// Making this duration longer will make the mesh stabilize more quickly when new peers join, but
/// take longer to evict unresponsive peers when they leave.
const MAX_PEER_SHARE_AGE: Duration = Duration::from_millis(10000);

/// How many other peers to ask to ping an unresponsive peer on our behalf.
const NUM_INDIRECT_PING_PEERS: usize = 3;

/// How many of our longest-since-heard-from peers to use as a potential candidate for our next
/// ping; we'll choose one of these at random. Choosing a random peer from a set of the N oldest
/// helps speed up down peer discovery in large meshes that all spun up at similar times.
const NUM_CANDIDATE_TARGET_PEERS: usize = 5;

/// How long to wait for an ack to a ping or indirect ping before assuming it's never going to come.
/// Per the SWIM paper, this could be based on an estimate of the distribution of round-trip
/// time in the network, e.g. an average or the 99th percentile.
const PING_TIMEOUT: Duration = Duration::from_millis(2000);

/// How often to re-broadcast our Join message if we don't know about any other peers right now.
const REBROADCAST_INTERVAL: Duration = Duration::from_millis(10000);

/// Generates a CRC32 hash of the given list of peers.
/// Because our only goal is to detect differences between two peer's understanding of the mesh,
/// rather than guard against malicious tampering, CRC32 is a good fit: it's sufficiently robust for
/// the task at hand, fast to compute, and has a very compact representation (a single u32).
pub fn generate_fingerprint(known_peers: &KnownPeers) -> u32 {
    let mut hosts: Vec<String> = known_peers.keys().map(|peer| peer.to_string()).collect();
    hosts.sort();
    crc32fast::hash(hosts.join(",").as_bytes())
}

pub struct StartResult {
    pub self_addr: SocketAddr,
    pub cancellation_tx: Sender<()>,
}

pub struct KaboodleInner {
    rng: ChaChaRng,
    sock: UdpSocket,
    broadcast_addr: SocketAddr,
    broadcast_out_sock: UdpSocket,
    broadcast_in_sock: UdpSocket,
    self_addr: SocketAddr,
    known_peers: Arc<Mutex<KnownPeers>>,
    /// Maps from a peer's address to a list of other peers who would like to be informed if we get
    /// a ping ack from said peer.
    curious_peers: HashMap<Peer, Vec<Peer>>,
    /// Keeps track of when we last broadcast a Join message
    last_broadcast_time: Option<Instant>,
    /// Whether we should stop running; takes effect in the next tick
    cancellation_rx: Receiver<()>,
    /// Small payload to uniquely identity this instance to its peers; used to allow consumers of
    /// Kaboodle to keep track of a durable instance identity across sessions.
    identity: Bytes,
}

impl KaboodleInner {
    pub async fn start(
        interface: &Interface,
        broadcast_port: u16,
        known_peers: Arc<Mutex<KnownPeers>>,
        identity: Bytes,
    ) -> Result<StartResult, KaboodleError> {
        // Set up our main communications socket
        let sock = {
            let ip = interface.ip();
            UdpSocket::bind(format!("{ip}:0")).await?
        };

        // Put our socket address into the known peers list
        let self_addr = sock.local_addr().unwrap();
        known_peers.lock().await.insert(
            self_addr,
            PeerInfo {
                identity: identity.clone(),
                state: PeerState::Known(Instant::now()),
            },
        );

        // Set up our broadcast communications sockets
        let (broadcast_in_sock, broadcast_out_sock, broadcast_addr) =
            create_broadcast_sockets(interface, &broadcast_port)?;

        let (cancellation_tx, cancellation_rx) = tokio::sync::mpsc::channel(1);

        let mut instance = KaboodleInner {
            sock,
            self_addr,
            rng: ChaChaRng::from_entropy(),
            broadcast_addr,
            broadcast_in_sock,
            broadcast_out_sock,
            known_peers,
            curious_peers: HashMap::new(),
            last_broadcast_time: None,
            cancellation_rx,
            identity,
        };

        tokio::spawn(async move {
            instance.run().await;
        });

        Ok(StartResult {
            self_addr,
            cancellation_tx,
        })
    }

    /// Broadcasts the given message to the entire mesh.
    async fn broadcast_msg(&self, msg: &SwimBroadcast) -> Result<(), KaboodleError> {
        let out_bytes = bincode::serialize(&msg).expect("Failed to serialize");
        log::info!("BROADCAST {msg:?} to {:?}", self.broadcast_addr);
        self.broadcast_out_sock
            .send_to(&out_bytes, self.broadcast_addr)
            .await?;
        Ok(())
    }

    /// Sends the given mesh to a single specific peer.
    async fn send_msg(
        &self,
        target_peer: &SocketAddr,
        msg: &SwimMessage,
    ) -> Result<(), KaboodleError> {
        log::info!("SEND [{target_peer}] {msg:?}");
        let env = SwimEnvelope {
            identity: self.identity.clone(),
            msg: msg.clone(),
        };
        let out_bytes = bincode::serialize(&env).expect("Failed to serialize");
        self.sock.send_to(&out_bytes, target_peer).await?;
        Ok(())
    }

    async fn maybe_broadcast_join(&mut self) {
        let now = Instant::now();
        if let Some(last_broadcast_time) = self.last_broadcast_time {
            // Re-broadcast if we only know about ourself and it has been a while since we tried
            let known_peers = self.known_peers.lock().await;
            if now.duration_since(last_broadcast_time) < REBROADCAST_INTERVAL
                || known_peers.len() > 1
            {
                return;
            }
        }

        // Broadcast our existence
        self.last_broadcast_time = Some(now);
        if let Err(err) = self
            .broadcast_msg(&SwimBroadcast::Join {
                addr: self.self_addr,
                identity: self.identity.clone(),
            })
            .await
        {
            log::warn!("Failed to broadcast our join message: {err:?}");
        }
    }

    /// Handle any incoming broadcast messages
    /// Note that, at least on macOS, if you run multiple copies of this app simultaneously, only
    /// one instance will actually receive broadcast packets.
    async fn handle_incoming_broadcasts(&mut self) {
        let mut buf = [0; INCOMING_BUFFER_SIZE];
        while let Ok((_len, sender)) = self.broadcast_in_sock.try_recv_from(&mut buf) {
            let Ok(msg) = bincode::deserialize::<SwimBroadcast>(&buf) else {
                // This can happen if there are multiple incompatible versions of Kaboodle running
                // at the same time -- e.g. if we've introduced a breaking change to the
                // SwimBroadcast enum.
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
                SwimBroadcast::Join { addr, identity } => {
                    if addr == self.self_addr {
                        continue;
                    }
                    log::info!("Got a join from {addr}");

                    let peer_info = PeerInfo {
                        identity: identity.clone(),
                        state: PeerState::Known(Instant::now()),
                    };
                    let mut known_peers = self.known_peers.lock().await;
                    known_peers.insert(addr, peer_info);
                    drop(known_peers);

                    // todo: maybe only send this if `is_new_peer`? (this would be a behaviour
                    // change, so I am not doing it right now.)
                    self.maybe_send_known_peers_to_peer(addr).await;
                }
            }
        }
    }

    async fn maybe_send_known_peers_to_peer(&mut self, addr: SocketAddr) {
        let known_peers = self.known_peers.lock().await;

        // Figure out whether we should send the new peer our list of known peers
        // Everyone in the mesh receives these broadcasts, so we don't want to respond
        // 100% of the time -- otherwise, new peers in large meshes would receive an
        // avalanche of redundant peer lists from every other peer when they first show
        // up. The exact formula may get tweaked over time, but the intention is to
        // ramp down from a 100% chance if we don't know of any other peers yet to a
        // minimum of 1% once we have some sufficient number.
        // See Section 3.2 in the SWIM paper for more thoughts on this logic.
        let num_other_peers = known_peers.len() - 2; // minus ourselves and the newcomer
        let percent_chance_of_sending_peers =
            std::cmp::max(1, 100 - i64::pow(num_other_peers as i64, 2)) as f64 / 100.0;
        let should_send_peers = self.rng.gen_bool(percent_chance_of_sending_peers);
        if !should_send_peers {
            log::info!("Not sending known peers to new peer in the hopes that someone else will");
            return;
        }

        // Send a list of known peers to the newcomer
        // todo: we might need to only send a subset in order to keep packet size down;
        // that is a problem for another day.
        let other_peers: HashMap<Peer, Bytes> = known_peers
            .iter()
            .map(|(other_peer, other_peer_info)| {
                (other_peer.to_owned(), other_peer_info.identity.to_owned())
            })
            .collect();
        drop(known_peers);

        if !other_peers.is_empty() {
            if let Err(err) = self
                .send_msg(&addr, &SwimMessage::KnownPeers(other_peers))
                .await
            {
                // Log a warning so we know something went wrong, but there's not actually
                // anything to be done in this case -- so long as at least one other
                // member of the mesh succesfully sends a response to the newcomer, we
                // should be okay.
                log::warn!("Failed to send known peers to newly joined peer: {err:?}");
            }
        }
    }

    async fn handle_incoming_messages(&mut self) {
        let mut buf = [0; INCOMING_BUFFER_SIZE];
        while let Ok((_len, sender)) = self.sock.try_recv_from(&mut buf) {
            let Ok(env) = bincode::deserialize::<SwimEnvelope>(&buf) else {
                // This can happen if there are multiple incompatible versions of Kaboodle running
                // at the same time -- e.g. if we've introduced a breaking change to the SwimMessage
                // enum.
                log::warn!("Failed to deserialize bytes: {buf:?}");
                continue;
            };
            log::info!("RECV [{} ({})] {:?}", sender, env.identity.len(), env.msg);

            // Insert the peer into our known_peers map as Known; if they were already in
            // there in a WaitingFor... state, this will reset them back to being known.
            let mut known_peers = self.known_peers.lock().await;
            let peer_info = PeerInfo {
                identity: env.identity.clone(),
                state: PeerState::Known(Instant::now()),
            };
            known_peers.insert(sender, peer_info);
            drop(known_peers);

            match env.msg {
                SwimMessage::Ack {
                    peer,
                    mesh_fingerprint: their_fingerprint,
                    num_peers: their_num_peers,
                } => {
                    if let Some(observers) = self.curious_peers.remove(&peer) {
                        // Some of our peers were waiting to hear back about this ping
                        for observer in observers {
                            // todo: run these in parallel
                            if let Err(err) = self
                                .send_msg(
                                    &observer,
                                    &SwimMessage::Ack {
                                        peer,
                                        mesh_fingerprint: their_fingerprint,
                                        num_peers: their_num_peers,
                                    },
                                )
                                .await
                            {
                                log::warn!(
                                    "Failed to send indirect ping response {observer}: {err:?}"
                                );
                            }
                        }
                    }

                    self.maybe_sync_known_peers(peer, their_fingerprint, their_num_peers)
                        .await;
                }
                SwimMessage::KnownPeers(peers) => {
                    let mut known_peers = self.known_peers.lock().await;
                    // Insert all of the new-to-us peers with a timestamp that is intentionally too
                    // old for us to share them in any incoming KnownPeersRequests that we may
                    // receive; this guarantees that now-vanished peers don't get propagated around
                    // via KnownPeersRequest messages indefinitely.
                    let peers_to_add: HashMap<_, _> = peers
                        .into_iter()
                        .filter(|(peer, _)| !known_peers.contains_key(peer))
                        .collect();

                    let too_old_to_share_timestamp =
                        Instant::now().checked_sub(MAX_PEER_SHARE_AGE).unwrap();
                    for (peer, identity) in peers_to_add {
                        known_peers.insert(
                            peer,
                            PeerInfo {
                                identity: identity.clone(),
                                state: PeerState::Known(too_old_to_share_timestamp),
                            },
                        );
                    }
                    drop(known_peers);
                }
                SwimMessage::KnownPeersRequest {
                    mesh_fingerprint: their_fingerprint,
                    num_peers: their_num_peers,
                } => {
                    let known_peers = self.known_peers.lock().await;

                    // Send back a list of every other peer (besides ourselves and the requestor)
                    // who is in the Known state and who we have heard from since
                    // MAX_PEER_SHARE_AGE. The state and age checks make us less likely to
                    // accidentally propagate echoes of down-but-not-yet-noticed-down nodes.
                    let other_peers = known_peers
                        .iter()
                        .filter_map(
                            |(other_peer, other_peer_info)| match other_peer_info.state {
                                PeerState::Known(last_pinged)
                                    if *other_peer != self.self_addr
                                        && *other_peer != sender
                                        && Instant::now().duration_since(last_pinged)
                                            < MAX_PEER_SHARE_AGE =>
                                {
                                    Some((
                                        other_peer.to_owned(),
                                        other_peer_info.identity.to_owned(),
                                    ))
                                }
                                _ => None,
                            },
                        )
                        .collect();
                    drop(known_peers);
                    if let Err(err) = self
                        .send_msg(&sender, &SwimMessage::KnownPeers(other_peers))
                        .await
                    {
                        log::warn!("Failed to reply to known peer request: {err:?}");
                    }

                    self.maybe_sync_known_peers(sender, their_fingerprint, their_num_peers)
                        .await;
                }
                SwimMessage::Ping => {
                    let known_peers = self.known_peers.lock().await;
                    let our_fingerprint = generate_fingerprint(&known_peers);
                    let our_num_peers = known_peers.len() as u32;
                    drop(known_peers);

                    if let Err(err) = self
                        .send_msg(
                            &sender,
                            &SwimMessage::Ack {
                                peer: self.self_addr,
                                mesh_fingerprint: our_fingerprint,
                                num_peers: our_num_peers,
                            },
                        )
                        .await
                    {
                        log::warn!("Failed to reply to known ping request: {err:?}");
                    }
                }
                SwimMessage::PingRequest(peer) => {
                    // Make a note of the fact that `sender` wants to hear whenever we get an ack
                    // back from `peer`.
                    let mut observers = self.curious_peers.remove(&peer).unwrap_or_default();
                    if !observers.contains(&sender) {
                        observers.push(sender);
                    }
                    self.curious_peers.insert(peer, observers);

                    if let Err(err) = self.send_msg(&peer, &SwimMessage::Ping).await {
                        log::warn!("Failed to send indirect ping on behalf of {sender:?}: {err:?}");
                    }
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
            .filter(|(peer, peer_info)| {
                **peer != self.self_addr && matches!(peer_info.state, PeerState::Known(_))
            })
            .map(|(peer, _)| *peer)
            .collect::<Vec<Peer>>();

        // Note: iterating over every known peer to find the ones in the WaitingFor... states is
        // obviously inefficient. If that turns out to be a problem, we can create separate lists
        // for those peers. Until it's known to be a problem, however, the simplicity of just having
        // a single HashMap remains appealling.
        for (peer, peer_info) in known_peers.iter() {
            match peer_info.state {
                PeerState::Known(_) => {
                    // Nothing required
                }
                PeerState::WaitingForPing(ping_sent) => {
                    if Instant::now().duration_since(ping_sent) < PING_TIMEOUT {
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
                        if let Err(err) = self.send_msg(indirect_ping_peer, &msg).await {
                            log::warn!("Failed to send indirect peer request to {indirect_ping_peer}: {err:?}");
                        }
                    }
                }
                PeerState::WaitingForIndirectPing(ping_sent) => {
                    if Instant::now().duration_since(ping_sent) < PING_TIMEOUT {
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
            let did_update = known_peers.update(&peer, |mut peer_info| {
                peer_info.state = PeerState::WaitingForIndirectPing(Instant::now());
                peer_info
            });
            if !did_update {
                log::warn!("Failed to update indirectly pinged peer state {peer}; this is a programming error");
            }
        }

        for removed_peer in removed_peers {
            log::info!("Removing peer {removed_peer}");
            known_peers.remove(&removed_peer);
            self.curious_peers.remove_entry(&removed_peer);

            if let Err(err) = self
                .broadcast_msg(&SwimBroadcast::Failed(removed_peer))
                .await
            {
                log::warn!("Failed to broadcast failure message about {removed_peer}: {err:?}");
            }
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
            .filter_map(|(peer, peer_info)| match peer_info.state {
                PeerState::Known(last_pinged) if *peer != self.self_addr => {
                    Some((*peer, last_pinged))
                }
                _ => None,
            })
            .collect::<Vec<(Peer, Instant)>>();

        // Find the peer with the oldest "Known" timestamp; this is the one that our knowledge of
        // is most out of date.
        non_suspected_peers.sort_by_key(|(_, last_pinged)| *last_pinged);
        let Some((target_peer, _)) = non_suspected_peers.iter().take(NUM_CANDIDATE_TARGET_PEERS).choose(&mut self.rng) else {
            // No one to ping
            return;
        };

        // - send a PING message to P and start a timeout
        //     - if P replies with an ACK, mark the peer as up
        let did_update = known_peers.update(target_peer, |mut peer_info| {
            peer_info.state = PeerState::WaitingForPing(Instant::now());
            peer_info
        });
        if !did_update {
            log::warn!("Failed to update randomly-selected peer state {target_peer}; this is a programming error");
            return;
        }
        drop(known_peers);

        // Comment out the following line to test indirect pinging
        if let Err(err) = self.send_msg(target_peer, &SwimMessage::Ping).await {
            // TODO: we could request an indirect ping from other peers, or remove target_peer and
            // broadcast SwimBroadcast::Failed to the mesh, but for now, let's just remove them from
            // our known_peers list and let the problem sort itself out naturally. If the peer is
            // genuinely gone, every other peer in the mesh will eventually figure that out on their
            // own.
            log::warn!("Failed to send ping to randomly-selected peer {target_peer}: {err:?}; assuming they're down immediately");
            let mut known_peers = self.known_peers.lock().await;
            known_peers.remove(target_peer);
        }
    }

    /// Compares our perspective on the state of the mesh to that of one of our peers, and possibly
    /// sends them a KnownPeersRequest if we believe they have more/better information than we do.
    async fn maybe_sync_known_peers(
        &mut self,
        peer: SocketAddr,
        their_fingerprint: u32,
        their_num_peers: u32,
    ) {
        let known_peers = self.known_peers.lock().await;
        let our_fingerprint = generate_fingerprint(&known_peers);
        let our_num_peers = known_peers.len() as u32;
        drop(known_peers);

        if our_fingerprint == their_fingerprint {
            return;
        }

        if our_num_peers > their_num_peers {
            // We know more about them than they know about us; they'll send us a KnownPeersRequest
            log::info!("maybe_sync_known_peers: expect to receive a KnownPeersRequest");
            return;
        }

        if let Err(err) = self
            .send_msg(
                &peer,
                &SwimMessage::KnownPeersRequest {
                    mesh_fingerprint: our_fingerprint,
                    num_peers: our_num_peers,
                },
            )
            .await
        {
            log::warn!("Failed to send known peers to {peer}: {err:?}");
        }
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

    pub async fn run(&mut self) {
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
