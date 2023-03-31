#![deny(unsafe_code)]
#![warn(missing_debug_implementations, trivial_casts, unused_qualifications)]

use bytes::Bytes;

use crate::kaboodle::generate_fingerprint;
use crate::observable_hashmap::Event;
use crate::structs::{Fingerprint, KnownPeers, Peer};
use std::sync::Arc;
use tokio::sync::{mpsc::UnboundedSender, Mutex};

pub type DiscoverySenders = Vec<UnboundedSender<(Peer, Bytes)>>;
pub type DepartureSenders = Vec<UnboundedSender<Peer>>;
pub type FingerprintSenders = Vec<UnboundedSender<Fingerprint>>;

/// Listens to changes on the given known_peers ObservableHashMap and turns them into higher-level
/// channel transmissions for peer arrivals, departures, and changes to the mesh fingerprint.
pub fn handle_known_peers_events(
    known_peers: Arc<Mutex<KnownPeers>>,
    discovery_tx: Arc<Mutex<DiscoverySenders>>,
    departure_tx: Arc<Mutex<DepartureSenders>>,
    fingerprint_tx: Arc<Mutex<FingerprintSenders>>,
) {
    /// Sends the given payload to the given list of channels.
    fn broadcast_to_channels<T>(payload: T, channels: &mut Vec<UnboundedSender<T>>)
    where
        T: Clone,
    {
        let mut closed_channels: Vec<usize> = vec![];
        for (idx, tx) in channels.iter().enumerate() {
            if tx.send(payload.clone()).is_err() {
                // Channel must be closed ¯\_(ツ)_/¯
                closed_channels.push(idx);
            }
        }
        for idx in closed_channels {
            channels.remove(idx);
        }
    }

    tokio::spawn(async move {
        let mut rx = {
            let mut known_peers = known_peers.lock().await;
            known_peers.add_observer()
        };

        let mut prev_fingerprint = 0;

        while let Some(event) = rx.recv().await {
            // We got an event; if there are more that are ready to receive, consume them all so we
            // can just send out a single fingerprint change event.
            let mut events = vec![event];
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }

            for event in events {
                match event {
                    Event::Added(addr) => {
                        let identity = {
                            let known_peers = known_peers.lock().await;
                            let Some(peer_info) = known_peers.get(&addr) else {
                                log::warn!("Received Event::Added but peer is not present in known_peers; this is a programming error");
                                continue;
                            };
                            peer_info.identity.clone()
                        };

                        // Any time Kaboodle::discover_peer is called, it creates a new channel and adds the
                        // sender to our `discovery_tx` map.
                        let mut discovery_tx = discovery_tx.lock().await;

                        log::debug!(
                            "New peer discovered; addr={addr}; identity={identity:?}; listeners={}",
                            discovery_tx.len()
                        );

                        broadcast_to_channels((addr, identity.clone()), &mut discovery_tx);
                    }
                    Event::Updated(addr, prev_value, new_value) => {
                        if prev_value.identity == new_value.identity {
                            // identity changes are the only thing that matters here; ignore if same
                            continue;
                        }

                        log::debug!("Peer updated; addr={addr}");
                    }
                    Event::Removed(addr) => {
                        let known_peers = known_peers.lock().await;
                        if known_peers.contains_key(&addr) {
                            // Well, they got added back right away, apparently, so ignore this
                            continue;
                        }

                        log::debug!("Peer left; addr={addr}");

                        let mut departure_tx = departure_tx.lock().await;
                        broadcast_to_channels(addr, &mut departure_tx);
                    }
                }
            }

            // If we didn't `continue` in one of the match arms, then send out a fingerprint change
            // notification.
            let mut fingerprint_tx = fingerprint_tx.lock().await;
            if fingerprint_tx.is_empty() {
                continue;
            }

            // Make sure known_peers isn't empty; if it is, then we got called after Kaboodle
            // was stopped when it happened to not know about any other peers. This isn't a
            // particularly helpful situation in which to broadcast a fingerprint change event,
            // and the fingerprint would be `0` anyhow, so don't bother.
            let known_peers = known_peers.lock().await;
            if known_peers.is_empty() {
                continue;
            }
            let fingerprint = generate_fingerprint(&known_peers);
            if fingerprint != prev_fingerprint {
                prev_fingerprint = fingerprint;
                broadcast_to_channels(fingerprint, &mut fingerprint_tx);
            }
        }
    });
}
