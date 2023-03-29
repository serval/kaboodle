use std::{io::Write, process::exit, thread::sleep, time::Duration};

use clap::Parser;
use dotenvy::dotenv;
use if_addrs::{IfAddr, Interface};
use kaboodle::{networking::non_loopback_interfaces, Kaboodle};

pub mod errors;
pub mod networking;

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
        println!("Debug-only warning: no .env file found to configure logging; all logging will be disabled. Add RUST_LOG=info to .env to see logging.");
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
    println!(
        "Identity: {}\x1B]0;{self_addr}\x07",
        args.identity.unwrap_or_else(|| String::from("(none)")),
    );
    println!("Address: {self_addr}");
    println!("Port: {}", args.port);

    let mut discovery_rx = kaboodle.discover_peers().unwrap();
    let mut departure_rx = kaboodle.discover_departures().unwrap();
    let mut fingerprint_rx = kaboodle.discover_fingerprint_changes().unwrap();

    const SPINNER_FRAMES: &str = "⣾⣽⣻⢿⡿⣟⣯⣷";
    let mut spinner_frame: usize = 0;

    loop {
        let mut did_emit_output = false;

        // Check for new peers
        if let Ok((addr, identity_bytes)) = discovery_rx.try_recv() {
            let identity = String::from_utf8(identity_bytes.to_vec())
                .map(|id| format!(" ({id})"))
                .unwrap_or_default();
            if !did_emit_output {
                print!("\x0D\x0D"); // delete the last frame of the spinner
                did_emit_output = true;
            }
            println!("New peer: {addr}{identity}");
        }

        // Check for departed peers
        if let Ok(departed_peer) = departure_rx.try_recv() {
            if !did_emit_output {
                print!("\x0D\x0D"); // delete the last frame of the spinner
                did_emit_output = true;
            }
            println!("Peer left: {departed_peer}");
        }

        // Check for fingerprint changes; we want to drain `fingerprint_rx` because multiple changes
        // may have happened since we last checked, but we only care about the current value.
        let mut new_fingerprint = None;
        while let Ok(fingerprint) = fingerprint_rx.try_recv() {
            new_fingerprint = Some(fingerprint);
        }
        if let Some(fingerprint) = new_fingerprint {
            if !did_emit_output {
                print!("\x0D\x0D"); // delete the last frame of the spinner
            }

            let known_peers = kaboodle.peer_states().await;
            let num_peers = known_peers.len();
            let title = format!("\x1B]0;{self_addr} {num_peers} {fingerprint:08x}\x07");
            println!(
                "Mesh fingerprint is now {fingerprint:08x} with {num_peers} peers in mesh:{title}"
            );
            for (peer, peer_info) in known_peers.iter() {
                let identity = String::from_utf8(peer_info.identity.to_vec())
                    .map(|id| format!(" ({id})"))
                    .unwrap_or_default();
                println!("+ {peer}{identity}");
            }
        }

        // Kaboodle only does work every second, which means there's no value in trying to
        // receive data from the various channels more than once a second. However, we'd like to
        // have a nice, smooth spinner, so run ten frames of animation back-to-back:
        let num_frames = SPINNER_FRAMES.chars().count();
        for _ in 0..10 {
            print!(
                "\x0D\x0D{} ",
                SPINNER_FRAMES.chars().nth(spinner_frame).unwrap()
            );
            std::io::stdout().flush().unwrap();
            spinner_frame = (spinner_frame + 1) % num_frames;
            sleep(Duration::from_millis(100));
        }
    }
}
