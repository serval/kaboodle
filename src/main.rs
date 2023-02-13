// todo
// - proper error handling
// - add a 'props' payload for peers to share info about themsleves
// - re-send the join message every N seconds if I have no peerrs

use std::{
    collections::HashMap,
    fmt::Display,
    net::SocketAddr,
    thread::sleep,
    time::{Duration, Instant},
};

use dotenvy::dotenv;
use networking::my_ipv4_addrs;

use rand::{seq::SliceRandom, thread_rng};
use serde_derive::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::net::UdpSocket;

mod networking;

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

async fn send_msg(sock: &UdpSocket, target_peer: &SocketAddr, msg: &SwimMessage) {
    log::info!("SEND [{target_peer}] {msg:?}");
    let out_bytes = bincode::serialize(&msg).expect("Failed to serialize");
    sock.send_to(&out_bytes, target_peer)
        .await
        .expect("Failed to send");
}

fn set_terminal_title(title: &str) {
    println!("\x1B]0;{title}\x07");
}

/// The minimum amount of time to wait between ticks; we keep track of how long it's been since the
/// start of the current tick and wait however long is required to keep the time between ticks as
/// close as possible to this duration.
const IDEAL_TICK_DURATION: Duration = Duration::from_millis(1000);

/// How many other peers to ask to ping an unresponsive peer on our behalf.
const NUM_INDIRECT_PING_PEERS: usize = 3;

/// How long to wait for an ack to a ping or indirect ping before assuming it's never going to come.
const PING_TIMEOUT: Duration = Duration::from_millis(2000);

#[derive(Debug, Eq, PartialEq)]
enum PeerState {
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

#[tokio::main]
async fn main() {
    let did_find_dotenv = dotenv().ok().is_some();
    if cfg!(debug_assertions) && !did_find_dotenv {
        log::info!("Debug-only warning: no .env file found to configure logging; all logging will be disabled. Add RUST_LOG=info to .env to see logging.");
    }
    env_logger::init();

    let mut rng = thread_rng();

    // Maps from a peer's address to the known state of that peer. See PeerState for a description
    // of the individual states.
    let mut known_peers: HashMap<Peer, PeerState> = HashMap::new();

    // Maps from a peer's address to a list of other peers who would like to be informed if we get
    // a ping ack from said peer.
    let mut curious_peers: HashMap<Peer, Vec<Peer>> = HashMap::new();

    // Set up our main communications socket
    let sock = {
        let ip = my_ipv4_addrs()[0];
        UdpSocket::bind(format!("{ip}:0"))
            .await
            .expect("Failed to bind")
    };
    let self_addr = sock.local_addr().unwrap();

    // Set up our broadcast communications socket
    let broadcast_addr: SocketAddr = "0.0.0.0:7475".parse().unwrap();
    let broadcast_sock = {
        let broadcast_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
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

    set_terminal_title(&self_addr.to_string());
    log::info!("I am {self_addr}");
    known_peers.insert(self_addr, PeerState::Known);

    // Broadcast our existence
    send_msg(
        &broadcast_sock,
        &broadcast_addr,
        &SwimMessage::Join(self_addr),
    )
    .await;

    loop {
        let tick_start = Instant::now();

        // Handle any broadcast messages
        // Note that, at least on macOS, if you run multiple copies of this app simultaneously, only
        // one instance will actually receive broadcast packets.
        loop {
            let mut buf = [0; 1024];
            let Ok((_len, sender)) = broadcast_sock.try_recv_from(&mut buf) else {
                // Nothing to receive
                break;
            };
            if sender == self_addr {
                // Ignore our own broadcasts
                continue;
            };
            let Ok(msg) = bincode::deserialize::<SwimMessage>(&buf) else {
                log::warn!("Failed to deserialize bytes: {buf:?}");
                continue;
            };
            log::info!("CAST [{sender}] {msg:?}");
            match msg {
                SwimMessage::Failed(peer) => {
                    if peer == self_addr {
                        // Someone else must've lost connectivity to us, but that doesn't mean we
                        // should forget about ourselves.
                        continue;
                    };

                    // Note: unclear whether we should unilaterally trust this but ok
                    log::info!("Removing peer that we were told has failed {peer}");
                    known_peers.remove(&peer);
                }
                SwimMessage::Join(peer) => {
                    log::info!("Got a join from {peer}");
                    known_peers.insert(peer, PeerState::Known);

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

                    if !other_peers.is_empty() {
                        send_msg(&sock, &peer, &SwimMessage::KnownPeers(other_peers)).await;
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
            let Ok((_len, sender)) = sock.try_recv_from(&mut buf) else {
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
                    known_peers.insert(peer, PeerState::Known);

                    if let Some(observers) = curious_peers.remove(&peer) {
                        // Some of our peers were waiting to hear back about this ping
                        for observer in observers {
                            // todo: run these in parallel
                            send_msg(&sock, &observer, &SwimMessage::Ack(peer)).await;
                        }
                    }
                }
                SwimMessage::KnownPeers(peers) => {
                    for peer in peers {
                        known_peers.entry(peer).or_insert(PeerState::Known);
                    }
                }
                SwimMessage::Ping => {
                    send_msg(&sock, &sender, &SwimMessage::Ack(self_addr)).await;
                    known_peers.insert(sender, PeerState::Known);
                }
                SwimMessage::PingRequest(peer) => {
                    // Make a note of the fact that `sender` wants to hear whenever we get an ack
                    // back from `peer`.
                    let mut observers = curious_peers.remove(&peer).unwrap_or_default();
                    if !observers.contains(&sender) {
                        observers.push(sender);
                    }
                    curious_peers.insert(peer, observers);

                    send_msg(&sock, &peer, &SwimMessage::Ping).await;
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
        let non_suspected_peers = known_peers
            .iter()
            .filter(|(_, peer_state)| **peer_state == PeerState::Known)
            .map(|(peer, _)| *peer)
            .collect::<Vec<Peer>>();
        for (peer, peer_state) in known_peers.iter() {
            match peer_state {
                PeerState::WaitingForPing(ping_sent) => {
                    if Instant::now().duration_since(*ping_sent) < PING_TIMEOUT {
                        continue;
                    };

                    let indirect_ping_peers: Vec<&Peer> = non_suspected_peers
                        .choose_multiple(&mut rng, NUM_INDIRECT_PING_PEERS)
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
                        send_msg(&sock, indirect_ping_peer, &msg).await;
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
            known_peers.insert(peer, PeerState::WaitingForIndirectPing(Instant::now()));
        }
        for removed_peer in removed_peers {
            log::info!("Removing peer {removed_peer}");
            known_peers.remove(&removed_peer);
            curious_peers.remove_entry(&removed_peer);

            send_msg(
                &broadcast_sock,
                &broadcast_addr,
                &SwimMessage::Failed(removed_peer),
            )
            .await;
        }

        // Ping a random peer
        // - pick a known peer P at random
        let non_suspected_peers: Vec<Peer> = known_peers
            .iter()
            .filter(|(addr, peer_state)| **peer_state == PeerState::Known && **addr != self_addr)
            .map(|(addr, _)| *addr)
            .collect();
        if let Some(target_peer) = non_suspected_peers.choose(&mut rng) {
            // - send a PING message to P and start a timeout
            //     - if P replies with an ACK, mark the peer as up
            known_peers.insert(*target_peer, PeerState::WaitingForPing(Instant::now()));

            // Comment out the following line to test indirect pinging
            send_msg(&sock, target_peer, &SwimMessage::Ping).await;
        }

        // Dump our list of peers out
        if !known_peers.is_empty() {
            log::info!("== Peers: ========");
            for (peer, peer_state) in known_peers.iter() {
                log::info!("+ {peer}:\t{peer_state}");
            }
        }

        // Wait until the next tick
        let time_since_tick_start = Instant::now().duration_since(tick_start);
        let required_delay = IDEAL_TICK_DURATION - time_since_tick_start;
        if required_delay.as_millis() > 0 {
            sleep(required_delay);
        }
    }
}
