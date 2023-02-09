#![allow(dead_code, unused_imports, unused_variables)]
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Deref,
    rc::Rc,
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use mdns::{advertise_service, discover_service};
use networking::{find_nearest_port, my_ipv4_addrs};
use rand::{seq::SliceRandom, thread_rng};
use serde_derive::{Deserialize, Serialize};
use tokio::{net::UdpSocket, sync::Mutex};
use uuid::Uuid;

mod mdns;
mod networking;

type Peer = SocketAddr;
#[derive(Serialize, Deserialize, Debug, Clone)]
enum SwimMessage {
    Join(Peer, usize),
    Ping,
    PingRequest(Peer),
    Ack(Peer),
    Failed(Peer),
}

struct GossipHashMap {}

impl GossipHashMap {
    // Updates the GossipHashMap, returning once an event has occured
    async fn recv_async() {}
}

async fn send_msg(target_peer: SocketAddr, msg: SwimMessage) {
    let sock = UdpSocket::bind("0.0.0.0:0").await.expect("Failed to bind");
    let out_bytes = bincode::serialize(&msg).expect("Failed to serialize");
    sock.connect(target_peer).await.expect("Failed to connect");
    sock.send(&out_bytes).await.expect("Failed to send");
    println!("[{target_peer}] {msg:?}");
}

#[tokio::main]
async fn main() {
    let mut rng = thread_rng();

    // Maps from a peer's address to an Option<Instant>. If the Option is Some(...), that tells us
    // when we started suspecting that the peer may be down. If the Option is None, we don't suspect
    // it.
    let mut known_peers: HashMap<SocketAddr, Option<Instant>> = HashMap::new();

    // Maps from a peer's address to a list of other peers who would like to be informed if we get
    // a ping ack from said peer.
    let mut curious_peers: HashMap<SocketAddr, Vec<SocketAddr>> = HashMap::new();

    let sock = UdpSocket::bind("0.0.0.0:0").await.expect("Failed to bind");
    let self_addr = sock.local_addr().unwrap();
    println!("I am {self_addr}");

    // Bootstrap:
    // - new node has to be aware of at least one peer P
    // - send a JOIN to P announcing your presence
    // todo!("Figure out an actual strategy for finding the initial peer");
    advertise_service("swim", self_addr.port(), &Uuid::new_v4(), None).unwrap();
    loop {
        println!("Bootstrap: waiting to discover initial peer");
        let self_addr_ipv4 = match self_addr.ip() {
            IpAddr::V4(ip) => ip,
            _ => panic!("No IPv4 address"),
        };

        let initial_peer = discover_service("_swim")
            .await
            .expect("Failed to find a peer");
        let initial_peer_ip_addrs = initial_peer.get_addresses().into_iter().collect::<Vec<_>>();
        if initial_peer_ip_addrs.contains(&&self_addr_ipv4) {
            continue;
        }
        let initial_peer_socket_addr: SocketAddr =
            format!("{}:{}", initial_peer_ip_addrs[0], initial_peer.get_port())
                .parse()
                .unwrap();

        println!("Discovered initial peer {initial_peer_socket_addr}");
        send_msg(initial_peer_socket_addr, SwimMessage::Join(self_addr, 3)).await;
        println!("Sent initial join message");

        break;
    }

    loop {
        let tick_start = Instant::now();

        // Handle any incoming messages
        loop {
            let mut buf = [0u8; 1024];
            let Ok((len, sender)) = sock.try_recv_from(&mut buf) else {
                // No more messages for now
                break;
            };
            let Ok(msg) = bincode::deserialize::<SwimMessage>(&buf) else {
                eprintln!("Failed to deserialize bytes: {buf:?}");
                continue;
            };

            match msg {
                SwimMessage::Ack(peer) => {
                    // Insert the peer into our known_peers map with None as their "suspected since"
                    // timestamp; if they were already in there with a suspected since timestamp,
                    // this will reset them back to being non-suspected.
                    let peer_prev = known_peers.insert(peer, None);
                    if let Some(peer_prev) = peer_prev {
                        if peer_prev.is_some() {
                            println!("Got ACK from previously-suspected peer {peer}");
                        }
                    } else {
                        println!("Got ACK from not-previously-known peer {peer}");
                    }

                    if let Some(observers) = curious_peers.remove(&peer) {
                        // Some of our peers were waiting to hear back about this ping
                        for observer in observers {
                            // todo: run these in parallel
                            send_msg(observer, SwimMessage::Ack(peer)).await;
                        }
                    }
                }
                SwimMessage::Failed(peer) => {
                    // Note: unclear whether we should unilaterally trust this but ok
                    known_peers.remove(&peer);
                }
                SwimMessage::Join(peer, ttl) => {
                    if known_peers.contains_key(&peer) {
                        break;
                    }

                    if ttl > 0 {
                        // Tell some of our peers about this
                        let out_msg = SwimMessage::Join(peer, ttl - 1);
                        let other_peers = Vec::from_iter(known_peers.keys());

                        for other_peer in other_peers.choose_multiple(&mut rng, 3) {
                            // todo: run these in parallel
                            send_msg(*other_peer.to_owned(), out_msg.clone()).await
                        }
                    }

                    known_peers.insert(peer, None);
                }
                SwimMessage::Ping => {
                    send_msg(sender, SwimMessage::Ack(self_addr)).await;
                }
                SwimMessage::PingRequest(peer) => {
                    // Make a note of the fact that `sender` wants to hear whenever we get an ack
                    // back from `peer`.
                    let mut observers = curious_peers.remove(&peer).unwrap_or_default();
                    if !observers.contains(&sender) {
                        observers.push(sender);
                    }
                    curious_peers.insert(peer, observers);

                    send_msg(peer, SwimMessage::Ping).await;
                }
            }
        }

        // Handle suspected peers
        // - for each suspected peer P,
        //     - if P has been suspected for more than X seconds
        //         - remove it from the membership list
        //         - broadcast a FAILED(P) message to the mesh
        let mut removed_peers: Vec<Peer> = vec![];
        for (peer, maybe_suspected_since) in known_peers.iter() {
            let Some(suspected_since) = *maybe_suspected_since else { continue };
            if Instant::now().duration_since(suspected_since) > Duration::from_secs(60) {
                // Give up on 'em
                println!("Suspected peer {peer} timed out and is presumed down; removing them");
                removed_peers.push(*peer);
            }
        }
        for removed_peer in removed_peers {
            known_peers.remove(&removed_peer);

            // Build a list of peers that we should tell that peer is down
            let mut peers_to_inform: HashSet<Peer> = HashSet::new();

            // First, include any of our known peers who are not themselves suspected of being down
            for (peer, maybe_suspected_since) in known_peers.iter() {
                if maybe_suspected_since.is_none() {
                    peers_to_inform.insert(*peer);
                }
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
                send_msg(peer, SwimMessage::Failed(removed_peer)).await;
            }
        }

        // Ping a random peer
        // - pick a known peer P at random
        let non_suspected_peers: Vec<SocketAddr> = known_peers
            .iter()
            .filter(|(addr, suspected_since)| suspected_since.is_none() && **addr != self_addr)
            .map(|(addr, _)| *addr)
            .collect();
        if let Some(target_peer) = non_suspected_peers.choose(&mut rng) {
            println!("Pinging random peer {target_peer}");
            // - send a PING message to P and start a timeout
            //     - if P replies with an ACK, mark the peer as up
            known_peers.insert(*target_peer, Some(Instant::now()));
            send_msg(*target_peer, SwimMessage::Ping).await;
        } else {
            println!("No random peers to ping");
        }
        //     - if the timeout fires without receiving an ACK:
        //         - start a second timeout
        //         - send pick N other random peers and send each of them a PING-REQ(P) message
        //         - each recipient sends their own PING to P and forwards any ACK to the requestor
        //         - if any peer ACKs, mark P as up
        //         - if the second timeout fires, mark P as suspected and note the current timestamp
        // TODO: implement this :point_up:

        // Wait until the next tick
        let max_delay = Duration::from_millis(1000);
        let time_since_tick_start = Instant::now().duration_since(tick_start);
        let required_delay = max_delay - time_since_tick_start;
        if required_delay.as_millis() > 0 {
            sleep(required_delay);
        }
    }
}
