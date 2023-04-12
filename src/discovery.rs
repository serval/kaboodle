use crate::errors::KaboodleError;
use crate::networking::create_broadcast_sockets;
use crate::structs::{SwimBroadcast, SwimEnvelope};
use bytes::Bytes;
use if_addrs::Interface;
use tokio::time::timeout;

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;

/// How large a buffer to use when reading from our sockets; messages larger than this will be
/// truncated.
const INCOMING_BUFFER_SIZE: usize = 1024;

/// How long to wait before we re-broadcast our Probe message if we don't get a response.
const START_REBROADCAST_INTERVAL: Duration = Duration::from_millis(1000);

/// How much longer to wait for subsequent re-broadcasts.
const REBROADCAST_INTERVAL_MULTIPLIER: f64 = 1.25;

/// Maximum amount of time to ever wait in between re-broadcasts.
const MAX_REBROADCAST_INTERVAL: Duration = Duration::from_millis(10000);

/// Discovers one member of the mesh on the given port and interface, without actually joining
/// the mesh. Use this if you simply need to discover a member of the mesh but do not want to
/// join the mesh yourself.
pub async fn discover_mesh_member(
    broadcast_port: u16,
    interface: Interface,
) -> Result<(SocketAddr, Bytes), KaboodleError> {
    let sock = {
        let ip = interface.ip();
        UdpSocket::bind(format!("{ip}:0")).await?
    };
    let self_addr = sock
        .local_addr()
        .expect("Failed to get local socket address");
    let (_broadcast_in_sock, broadcast_out_sock, broadcast_addr) =
        create_broadcast_sockets(&interface, &broadcast_port)?;

    let mut rebroadcast_interval = START_REBROADCAST_INTERVAL;

    let mut buf = [0; INCOMING_BUFFER_SIZE];
    let mut last_broadcast_time = Instant::now() - rebroadcast_interval - Duration::from_secs(1);
    let out_bytes =
        bincode::serialize(&SwimBroadcast::Probe(self_addr)).expect("Failed to serialize");

    loop {
        if last_broadcast_time <= Instant::now() - rebroadcast_interval {
            log::debug!("Broadcasting probe message");
            last_broadcast_time = Instant::now();
            broadcast_out_sock
                .send_to(&out_bytes, &broadcast_addr)
                .await?;
        }

        let res = match timeout(rebroadcast_interval, sock.recv_from(&mut buf)).await {
            Err(_) => {
                // No response in time; send another probe out and try again
                let interval_ms =
                    (rebroadcast_interval.as_millis() as f64) * REBROADCAST_INTERVAL_MULTIPLIER;
                rebroadcast_interval = Duration::min(
                    MAX_REBROADCAST_INTERVAL,
                    Duration::from_millis(interval_ms as u64),
                );
                continue;
            }
            Ok(res) => res,
        };
        let Ok((_len, sender)) = res else {
            // Failed to read for some reason; this probably means that our socket got closed out
            // from underneath us somehow, and it's not clear how we should deal with that.
            // todo: figure out what to do if our socket closes
            continue;
        };

        // Someone responded to our request
        let Ok(env) = bincode::deserialize::<SwimEnvelope>(&buf) else {
            // This would happen if there had been an incompatible change to SwimEnvelope between
            // our code and which mesh member responded to us.
            continue;
        };

        return Ok((sender, env.identity));
    }
}
