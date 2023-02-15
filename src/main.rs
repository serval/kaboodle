use std::{thread::sleep, time::Duration};

use dotenvy::dotenv;
use kaboodle::Kaboodle;

pub mod networking;

fn set_terminal_title(title: &str) {
    print!("\x1B]0;{title}\x07");
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
    kaboodle.start().await;

    let Some(self_addr) = &kaboodle.self_addr() else {
        panic!("Expected us to have a self address by now");
    };
    set_terminal_title(&self_addr.to_string());
    log::info!("I am {self_addr}");

    loop {
        // Dump our list of peers out
        let known_peers = kaboodle.peer_states().await;
        if known_peers.len() < 2 {
            //  Note: we're always in the list of peers, hence the length check rather than an .is_empty()
            log::info!("== Peers: none");
            set_terminal_title(&self_addr.to_string());
        } else {
            let num_peers = known_peers.len();
            let fingerprint = kaboodle.fingerprint().await;
            let fingerprint = &fingerprint[0..8];
            log::info!("== Peers: {} ({})", num_peers, fingerprint);
            let num_peers_to_print = 8;
            for (peer, peer_state) in known_peers.iter().take(num_peers_to_print) {
                log::info!("+ {peer}:\t{peer_state}");
            }
            if num_peers > num_peers_to_print {
                log::info!("+ ... and {} more", num_peers - num_peers_to_print);
            }
            set_terminal_title(&format!("{self_addr} {num_peers} {fingerprint}"));
        }

        sleep(Duration::from_millis(1000));
    }
}
