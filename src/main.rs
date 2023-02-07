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
    Join(Peer),
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
    struct PeerInfo {
        addr: SocketAddr,
        suspected_since: Option<Instant>,
    }
    let known_peers: HashMap<SocketAddr, Option<Instant>>;

    // Bootstrap:
    // - new node has to be aware of at least one peer P
    // - send a JOIN to P announcing your presence
    //     - if you are a peer who receives a JOIN message from a peer you did not already know about,
    //         - decrement the TTL and if it is > 0, send a copy of the message (with the decremented TTL) to N randomly selected peers
    let initial_peer: SocketAddr = "127.0.0.1:1234".parse().unwrap();
    let sock = UdpSocket::bind("0.0.0.0:0").await.expect("Failed to bind");
    let self_addr = sock.local_addr().unwrap();
    send_msg(initial_peer, SwimMessage::Join(self_addr)).await;

    loop {
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
                SwimMessage::Ack(peer) => {}
                SwimMessage::Failed(peer) => {}
                SwimMessage::Join(peer) => {}
                SwimMessage::Ping => {}
                SwimMessage::PingRequest(peer) => {}
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
    }
}
