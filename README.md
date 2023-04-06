# Kaboodle

![Main branch checks](https://github.com/serval/kaboodle/actions/workflows/main.yml/badge.svg)
![Rust Version][rustc-image]
[![crates.io][crate-image]][crate-link]
[![Documentation][docs-image]][docs-link]
[![Dependency Status][deps-image]][deps-link]

Kaboodle is a Rust crate containing an approximate implementation of the [SWIM membership gossip protocol](http://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf), give or take some details. It can be used to discover other peers on a LAN without any central coordination, accurately keeping track of membership as machines come and go over time.

## Concepts & features

Wikipedia has [a brief overview of the SWIM protocol](https://en.wikipedia.org/wiki/SWIM_Protocol); we will not go into the details of how the protocol works here.

Kaboodle works by sending UDP multicast messages on a particular port number to discover potential peers on the local network. The port number you choose is up to you; it just needs to be the same for all of the peers that want to discover each other. Disjoint applications using Kaboodle can run independently on the same network so long as they use distinct port numbers.

You can optionally tell Kaboodle which network interface to use. By default, it will choose the first non-loopback IPv6 interface; if none are available, it will choose the first non-loopback IPv6 interface.

Any given running copy of Kadoodle is called an instance. Kaboodle doesn't know anything about the instance—it has no durable identity over time and is just a peer participating in the mesh. Depending on what you're going to build on top of Kaboodle, you may need some sort of durable identity (e.g. a machine ID of some kind) that you can access. To facilitate this, you can provide Kabdoodle with an identity at instantiation time. Kaboodle identities are [Bytes](https://crates.io/crates/bytes) instances and are treated as completely opaque blobs by the system; their meaning is entirely up to you.

## Inspecting the mesh

Given sufficient time, all of the peers in a given mesh should converge on the same perspective of the mesh—that is, every peer should know about every other peer, and they should all notice when a peer joins or leaves the mesh. Kaboodle keeps track of a "fingerprint" for the mesh, which it uses to make sure all of the peers agree on things.

At any point, you can retrieve the mesh's fingerprint and a list of all known peers:

```rust
let fingerprint = kaboodle.fingerprint().await;
let peers = kaboodle.peers().await;
println!("{fingerprint} {peers:?}");
```

The fingerprint is an CRC-32 hash of the membership of the mesh from the current instance's perspective; all machines in the mesh should converge on the same hash value fairly rapidly. The list of peers is a `HashMap<SocketAddr, Bytes>` mapping from the peer's Kaboodle socket address to their identity. Note that the port numbers in these `SocketAddr` values is distinct from the broadcast port number: all peers listen on the broadcast port, but they also send one-to-one messages to each other on a separate port.

## Determining node proximity

Kaboodle comes with an optional feature called `proximity`, which uses [violin](https://github.com/kbknapp/violin) to create a notion of node proximity between peers. This feature does not produce any _additional_ network traffic, however it will increase the size of existing Kaboodle payloads, which is why it is not enabled by default.


## Caveats & known issues

This crate is still at an early stage of active development and is likely not yet fit for production usage. Pull requests, feature requests, and feedback are all very much welcome.

## Development

This project uses [just](https://github.com/casey/just) (`brew install just`) for development workflows and automation. Run `just` with no arguments to see a list of available commands.

`cargo run` will run a demo application that looks for peers via port 7475. If you have [zellij](https://zellij.dev) (`brew install zellij`) installed, you can run `just run2x2` to run four copies of the demo app in a 2x2 grid. Try quitting some of the instances and see how the remaining instances discover their absence; each pane has a title above it showing that instance's IP address and port number, as well as the mesh fingerprint from the perspective of that node.

## LICENSE

[BSD-2-Clause-Patent](./LICENSE)

[//]: # (badges)

[rustc-image]: https://img.shields.io/badge/rustc-1.68+-blue.svg
[crate-image]: https://img.shields.io/crates/v/kaboodle.svg
[crate-link]: https://crates.io/crates/kaboodle
[docs-image]: https://docs.rs/kaboodle/badge.svg
[docs-link]: https://docs.rs/kaboodle
[deps-image]: https://deps.rs/repo/github/serval/kaboodle/status.svg
[deps-link]: https://deps.rs/repo/github/serval/kaboodle