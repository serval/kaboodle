use std::{process::exit, thread::sleep, time::Duration};

use dotenvy::dotenv;
use kaboodle::Kaboodle;

pub mod networking;

fn set_terminal_title(title: &str) {
    println!("\x1B]0;{title}\x07");
}

#[tokio::main]
async fn main() {
    let did_find_dotenv = dotenv().ok().is_some();
    if cfg!(debug_assertions) && !did_find_dotenv {
        log::info!("Debug-only warning: no .env file found to configure logging; all logging will be disabled. Add RUST_LOG=info to .env to see logging.");
    }
    env_logger::init();

    let mut kaboodle = Kaboodle::new(7475);

    // Begin discovering peers
    if let Err(err) = kaboodle.start().await {
        log::error!("Failed to start: {err:?}");
        exit(1);
    }

    let Some(self_addr) = &kaboodle.self_addr() else {
        panic!("Expected us to have a self address by now");
    };
    set_terminal_title(&self_addr.to_string());
    log::info!("I am {self_addr}");

    let mut prev_title = String::from("");

    loop {
        // Dump our list of peers out
        let known_peers = kaboodle.peer_states().await;

        let num_peers = known_peers.len();
        let fingerprint = kaboodle.fingerprint().await;
        log::info!("== Peers: {} ({:08x})", num_peers, fingerprint);
        let num_peers_to_print = usize::max_value(); // adjust this downward to only show a subset
        for (peer, peer_state) in known_peers.iter() {
            log::info!("+ {peer}:\t{peer_state}");
        }
        if num_peers > num_peers_to_print {
            log::info!("+ ... and {} more", num_peers - num_peers_to_print);
        }
        let title = format!("{self_addr} {num_peers} {fingerprint}");
        if title != prev_title {
            set_terminal_title(&title);
            prev_title = title;
        }

        sleep(Duration::from_millis(1000));
    }
}
