#![allow(dead_code, unused_imports, unused_variables)]
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    ops::Deref,
    rc::Rc,
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use networking::{find_nearest_port, my_ipv4_addrs};
use rand::{seq::SliceRandom, thread_rng};
use serde_derive::{Deserialize, Serialize};
use tokio::{net::UdpSocket, sync::Mutex};
use uuid::Uuid;

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
    sock.send(&out_bytes).await.expect("Failed to send");
}

#[tokio::main]
async fn main() {
    let mut known_peers: HashMap<SocketAddr, Option<Instant>> = HashMap::new();
    let mut rng = thread_rng();
    let mut curious_peers: HashMap<SocketAddr, Vec<SocketAddr>> = HashMap::new();

    // Bootstrap:
    // - new node has to be aware of at least one peer P
    // - send a JOIN to P announcing your presence
    //     - if you are a peer who receives a JOIN message from a peer you did not already know about,
    //         - decrement the TTL and if it is > 0, send a copy of the message (with the decremented TTL) to N randomly selected peers
    let initial_peer: SocketAddr = "127.0.0.1:1234".parse().unwrap();
    let sock = UdpSocket::bind("0.0.0.0:0").await.expect("Failed to bind");
    let self_addr = sock.local_addr().unwrap();
    send_msg(initial_peer, SwimMessage::Join(self_addr, 3)).await;

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
                    let peer_prev = known_peers.insert(sender, None);
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
                    // note: unclear whether we should unilaterally trust this but ok
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

        // Ping a random peer
        // - pick a known peer P at random
        // - send a PING message to P and start a timeout
        //     - if P replies with an ACK, mark the peer as up
        //     - if the timeout fires without receiving an ACK:
        //         - start a second timeout
        //         - send pick N other random peers and send each of them a PING-REQ(P) message
        //         - each recipient sends their own PING to P and forwards any ACK to the requestor
        //         - if any peer ACKs, mark P as up
        //         - if the second timeout fires, mark P as suspected and note the current timestamp

        // Wait until the next tick
        let max_delay = Duration::from_millis(100);
        let time_since_tick_start = Instant::now().duration_since(tick_start);
        let required_delay = max_delay - time_since_tick_start;
        if required_delay.as_millis() > 0 {
            sleep(required_delay);
        }
    }
}
