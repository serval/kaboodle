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
    let mut kaboodle =
        Kaboodle::new(args.port, preferred_interface).expect("Failed to create Kaboodle");

    // Begin discovering peers
    if let Err(err) = kaboodle.start().await {
        log::error!("Failed to start: {err:?}");
        exit(1);
    }

    let self_addr = &kaboodle
        .self_addr()
        .expect("We should have a self address by now");
    set_terminal_title(&self_addr.to_string());
    log::info!("I am {self_addr}, broadcasting on {}", args.port);

    let mut prev_title = String::from("");

    loop {
        // Dump our list of peers out
        let known_peers = kaboodle.peer_states().await;

        let num_peers = known_peers.len();
        let fingerprint = kaboodle.fingerprint().await;
        log::info!("== Peers: {} ({:08x})", num_peers, fingerprint);
        for (peer, peer_info) in known_peers.iter() {
            log::info!("+ {peer}:\t{}", peer_info.state);
        }
        let title = format!("{self_addr} {num_peers} {fingerprint}");
        if title != prev_title {
            set_terminal_title(&title);
            prev_title = title;
        }

        sleep(Duration::from_millis(1000));
    }
}
