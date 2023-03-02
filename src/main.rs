use std::{process::exit, thread::sleep, time::Duration};

use clap::Parser;
use dotenvy::dotenv;
use if_addrs::{IfAddr, Interface};
use kaboodle::{networking::non_loopback_interfaces, Kaboodle};

pub mod errors;
pub mod networking;

fn set_terminal_title(title: &str) {
    println!("\x1B]0;{title}\x07");
}

fn get_interface(specified_interface: &str) -> Option<Interface> {
    match specified_interface {
        "ipv4" => non_loopback_interfaces()
            .into_iter()
            .find(|iface| matches!(iface.addr, IfAddr::V4(_))),
        "ipv6" => non_loopback_interfaces()
            .into_iter()
            .find(|iface| matches!(iface.addr, IfAddr::V6(_))),
        ip_or_name => {
            // Use the first interface we find where the interface name (e.g. `en0` or IP
            // address matches the argument. Note that we don't do any canonicalization on the
            // input value; for IPv6, addresses should be provided in their full, uncompressed
            // format.
            non_loopback_interfaces()
                .into_iter()
                .find(|iface| iface.addr.ip().to_string() == ip_or_name || iface.name == ip_or_name)
        }
    }
}

#[derive(Parser, Debug)]
struct Args {
    // identity is an arbitrary Bytes payload; for the purposes of this demo app, we'll just accept
    // a string on the command line.
    #[arg(long)]
    identity: Option<String>,
    // payload is an arbitrary Bytes payload; for the purposes of this demo app, we'll just accept
    // a string on the command line.
    #[arg(long)]
    payload: Option<String>,
    // An interface name or IP address, or the string 'ipv4' or 'ipv6' to pick the first
    // non-loopback interface of that kind.
    #[arg(long)]
    interface: Option<String>,
    // UDP port number to use for broadcast messages.
    #[arg(long, default_value = "7475")]
    port: u16,
}

#[tokio::main]
async fn main() {
    let did_find_dotenv = dotenv().ok().is_some();
    if cfg!(debug_assertions) && !did_find_dotenv {
        log::info!("Debug-only warning: no .env file found to configure logging; all logging will be disabled. Add RUST_LOG=info to .env to see logging.");
    }
    env_logger::init();

    let args = Args::parse();
    let preferred_interface = args.interface.as_ref().and_then(|str| get_interface(str));
    if args.interface.is_some() && preferred_interface.is_none() {
        log::error!(
            "Failed to find an interfacing matching {}",
            args.interface.unwrap()
        );
        exit(1);
    }

    // Kaboodle treats identity as an opaque blob of bytes; it has no meaning of its own. It is
    // provided as a way for you to give Kaboodle peers durable identities over time; for example,
    // you could generate a UUID, write it to disk for use between sessions, and use that as the
    // identity for each node. Alternatively, you could use something intrinsic to the machine, like
    // its host name or the MAC address of a network interface. For the purposes of this demo app,
    // we're just accepting it as a command line parameter.
    // This value is included in nearly all of Kaboodle's network traffic, so try to keep it as
    // compact as possible (on the order of < 100 bytes, ideally).
    let identity = args
        .identity
        .as_ref()
        .map(|str| str.to_owned())
        .unwrap_or_default();

    // Kaboodle nodes have a payload associated with them. Much like identity, the payload is an
    // opaque blob of bytes whose meaning entirely depends on the consumer of this library. Unlike
    // identity, which is shared with all nodes in the mesh automatically, the payload is only sent
    // in response to a direct request. This means that it can be used to share larger amounts of
    // data. That being said, it is still transmitted over UDP, so keeping it under a few hundred
    // bytes of data would be wise.
    let payload = args
        .payload
        .as_ref()
        .map(|str| str.to_owned())
        .unwrap_or_default();

    let mut kaboodle = Kaboodle::new(args.port, preferred_interface, identity, payload)
        .expect("Failed to create Kaboodle");

    // Begin discovering peers
    if let Err(err) = kaboodle.start().await {
        log::error!("Failed to start: {err:?}");
        exit(1);
    }

    let self_addr = &kaboodle
        .self_addr()
        .expect("We should have a self address by now");
    set_terminal_title(&self_addr.to_string());
    log::info!(
        "Identity: {}",
        args.identity.unwrap_or_else(|| String::from("(none)")),
    );
    log::info!("Address: {self_addr}");
    log::info!("Port: {}", args.port);

    let mut prev_title = String::from("");
    let mut did_request_payload = false;

    loop {
        // Dump our list of peers out
        let known_peers = kaboodle.peer_states().await;

        let num_peers = known_peers.len();
        let fingerprint = kaboodle.fingerprint().await;
        log::info!("== Peers: {} ({:08x})", num_peers, fingerprint);
        for (peer, peer_info) in known_peers.iter() {
            let identity = String::from_utf8(peer_info.identity.to_vec())
                .map(|id| format!(" ({id})"))
                .unwrap_or_default();
            log::info!("+ {peer}{identity}:\t{}", peer_info.state);
        }
        let title = format!("{self_addr} {num_peers} {fingerprint}");
        if title != prev_title {
            set_terminal_title(&title);
            prev_title = title;
        }

        if num_peers > 1 && !did_request_payload {
            if let Some(target_peer) = known_peers
                .iter()
                .find(|(peer, _)| *peer != self_addr)
                .map(|(peer, _)| *peer)
            {
                did_request_payload = true;
                log::info!("Requesting payload from the first peer we discovered {target_peer:?}");
                let payload_res = kaboodle.peer_payload(target_peer).await;
                log::info!("Got payload result from peer {target_peer:?}: {payload_res:?}");
            }
        }

        sleep(Duration::from_millis(1000));
    }
}
