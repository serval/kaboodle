use dotenvy::dotenv;
use gossip::Gossip;

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

    let mut gossip = Gossip::new(7475).await;
    let self_addr = &gossip.get_self_addr();
    set_terminal_title(&self_addr.to_string());
    log::info!("I am {self_addr}");

    loop {
        gossip.tick().await;

        // Dump our list of peers out
        let known_peers = gossip.get_peers();
        if !known_peers.is_empty() {
            let fingerprint = &gossip.get_fingerprint()[0..8];
            log::info!("== Peers: {} ({})", known_peers.len(), fingerprint);
            for (peer, peer_state) in known_peers.iter() {
                log::info!("+ {peer}:\t{peer_state}");
            }
            set_terminal_title(&format!("{self_addr} {fingerprint}"));
        } else {
            log::info!("== Peers: none");
            set_terminal_title(&self_addr.to_string());
        }
    }
}
