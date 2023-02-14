use std::{thread::sleep, time::Duration};

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
    kaboodle.start().await;

    let Some(self_addr) = &kaboodle.get_self_addr() else {
        panic!("Expected us to have a self address by now");
    };
    set_terminal_title(&self_addr.to_string());
    log::info!("I am {self_addr}");

    loop {
        // Dump our list of peers out
        let known_peers = kaboodle.get_peer_states().await;
        if known_peers.len() < 2 {
            //  Note: we're always in the list of peers, hence the length check rather than an .is_empty()
            log::info!("== Peers: none");
            set_terminal_title(&self_addr.to_string());
        } else {
            let fingerprint = kaboodle.get_fingerprint().await;
            let fingerprint = &fingerprint[0..8];
            log::info!("== Peers: {} ({})", known_peers.len(), fingerprint);
            for (peer, peer_state) in known_peers.iter() {
                log::info!("+ {peer}:\t{peer_state}");
            }
            set_terminal_title(&format!("{self_addr} {fingerprint}"));
        }

        sleep(Duration::from_millis(1000));
    }
}
