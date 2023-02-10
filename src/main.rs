// todo
// - send known_peers list in response to Join messages, maybe? this is tricky because we can't
//   send an arbitrarily large amount of data. perhaps we could send as many fit into 1024 bytes?

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    thread::sleep,
    time::{Duration, Instant},
};

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
    println!("SEND [{target_peer}] {msg:?}");
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

#[tokio::main]
async fn main() {
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
    println!("I am {self_addr}");

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
                eprintln!("Failed to deserialize bytes: {buf:?}");
                continue;
            };
            println!("CAST [{sender}] {msg:?}");
            match msg {
                SwimMessage::Join(peer) => {
                    println!("Got a join from {peer}");
                    known_peers.insert(peer, PeerState::Known);

                    // Send them as many of our known peers as possible
                    // This is a gross hack: we keep serializing a KnownPeers message with the
                    // contents of `other_peers`, dropping one of them until it's small enough to
                    // fit. I am sure there's a less stupid way of accomplishing this.
                    let mut other_peers: Vec<Peer> = known_peers
                        .iter()
                        .filter(|(other_peer, _)| **other_peer != peer)
                        .map(|(other_peer, _)| *other_peer)
                        .collect();

                    while !other_peers.is_empty()
                        && bincode::serialize(&SwimMessage::KnownPeers(other_peers.clone()))
                            .unwrap()
                            .len()
                            > 1024
                    {
                        other_peers.pop();
                    }

                    if !other_peers.is_empty() {
                        send_msg(&sock, &peer, &SwimMessage::KnownPeers(other_peers)).await;
                    }
                }
                _ => {
                    println!("Got unexpected broadcast {msg:?}");
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
                eprintln!("Failed to deserialize bytes: {buf:?}");
                continue;
            };
            println!("RECV [{sender}] {msg:?}");

            match msg {
                SwimMessage::Ack(peer) => {
                    // Insert the peer into our known_peers map with None as their "suspected since"
                    // timestamp; if they were already in there with a suspected since timestamp,
                    // this will reset them back to being non-suspected.
                    let peer_prev = known_peers.insert(peer, PeerState::Known);
                    match peer_prev {
                        Some(PeerState::WaitingForPing(_))
                        | Some(PeerState::WaitingForIndirectPing(_)) => {
                            println!("Got ACK from previously-suspected peer {peer}");
                        }
                        None => {
                            println!("Got ACK from not-previously-known peer {peer}");
                        }
                        _ => {}
                    };

                    if let Some(observers) = curious_peers.remove(&peer) {
                        // Some of our peers were waiting to hear back about this ping
                        for observer in observers {
                            // todo: run these in parallel
                            send_msg(&sock, &observer, &SwimMessage::Ack(peer)).await;
                        }
                    }
                }
                SwimMessage::Failed(peer) => {
                    // Note: unclear whether we should unilaterally trust this but ok
                    println!("Removing peer that we were told has failed {peer}");
                    known_peers.remove(&peer);
                }
                SwimMessage::KnownPeers(peers) => {
                    for peer in peers {
                        if !known_peers.contains_key(&peer) {
                            known_peers.insert(peer, PeerState::Known);
                        }
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
                    println!("Received unexpected message: {msg:?}");
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
                        println!("Not long enough");
                        continue;
                    };

                    let indirect_ping_peers: Vec<&Peer> = non_suspected_peers
                        .choose_multiple(&mut rng, NUM_INDIRECT_PING_PEERS)
                        .collect();

                    if indirect_ping_peers.is_empty() {
                        println!("No indirect peers to ask");
                        // There's no one we can ask for an indirect ping, so give up on this peer
                        // right away I guess.
                        removed_peers.push(*peer);
                        continue;
                    }

                    // Ask these peers to ping the suspected peer on our behalf
                    indirectly_pinged_peers.push(*peer);
                    let msg = SwimMessage::PingRequest(*peer);
                    println!("Asking indirect peers");
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
                    println!("Suspected peer {peer} timed out and is presumed down; removing them");
                    removed_peers.push(*peer);
                }
                _ => {}
            }
        }
        for peer in indirectly_pinged_peers {
            known_peers.insert(peer, PeerState::WaitingForIndirectPing(Instant::now()));
        }
        for removed_peer in removed_peers {
            println!("Removing peer {removed_peer}");
            known_peers.remove(&removed_peer);

            // Build a list of peers that we should tell that peer is down
            let mut peers_to_inform: HashSet<Peer> = HashSet::new();

            // First, include any of our known peers who are not themselves suspected of being down
            for peer in non_suspected_peers.iter() {
                peers_to_inform.insert(*peer);
            }

            // Next, add any peers that have asked us to ping the down peer on their behalf
            if let Some((_, previously_curious_peers)) = curious_peers.remove_entry(&removed_peer) {
                for peer in previously_curious_peers {
                    peers_to_inform.insert(peer);
                }
            }

            // Finally, actually inform them
            for peer in peers_to_inform {
                // todo: run in parallel
                send_msg(&sock, &peer, &SwimMessage::Failed(removed_peer)).await;
            }
        }

        // Ping a random peer
        // - pick a known peer P at random
        let non_suspected_peers: Vec<Peer> = known_peers
            .iter()
            .filter(|(addr, peer_state)| **peer_state == PeerState::Known && **addr != self_addr)
            .map(|(addr, _)| *addr)
            .collect();
        if let Some(target_peer) = non_suspected_peers.choose(&mut rng) {
            println!("Pinging random peer {target_peer}");
            // - send a PING message to P and start a timeout
            //     - if P replies with an ACK, mark the peer as up
            known_peers.insert(*target_peer, PeerState::WaitingForPing(Instant::now()));

            // Comment out the following line to test indirect pinging
            send_msg(&sock, target_peer, &SwimMessage::Ping).await;
        }

        // Dump our list of peers out
        if !known_peers.is_empty() {
            println!("+= Peers: ========");
            for (peer, peer_state) in known_peers.iter() {
                println!("|| {peer}:\t{peer_state:?}");
            }
            println!("+=================");
        }

        // Wait until the next tick
        let time_since_tick_start = Instant::now().duration_since(tick_start);
        let required_delay = IDEAL_TICK_DURATION - time_since_tick_start;
        if required_delay.as_millis() > 0 {
            sleep(required_delay);
        }
    }
}
