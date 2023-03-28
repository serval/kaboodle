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
    #[arg(long)]
    identity: Option<String>,
    #[arg(long)]
    interface: Option<String>,
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
    let identity = args
        .identity
        .as_ref()
        .map(|str| str.to_owned())
        .unwrap_or_default();

    let mut kaboodle =
        Kaboodle::new(args.port, preferred_interface, identity).expect("Failed to create Kaboodle");

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

    // Write out a message any time a new peer is discovered
    if let Ok(mut discovery_rx) = kaboodle.discover_peers() {
        tokio::spawn(async move {
            loop {
                if let Some(new_peer) = discovery_rx.recv().await {
                    log::info!("New peer discovered! {new_peer:?}");
                }
            }
        });
    }

    // Write out a message the first time a new peer is discovered
    if let Ok(discovery_rx) = kaboodle.discover_next_peer() {
        tokio::spawn(async move {
            if let Ok(new_peer) = discovery_rx.await {
                log::info!("First peer discovered! {new_peer:?}");
            }
        });
    }

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

        sleep(Duration::from_millis(1000));
    }
}
