# Kaboodle

Kaboodle is a Rust crate containing an approximate implementation of the [SWIM membership gossip protocol](http://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf), give or take some details. It can be used to discover other peers on the LAN without any central coordination.

## Concepts & features

Wikipedia has [a brief overview of the SWIM protocol](https://en.wikipedia.org/wiki/SWIM_Protocol); we will not go into the details of how the protocol works here.

Kaboodle needs to be instantiated with a broadcast port number:

```rust
let kaboodle = Kaboodle::new(7475);
kaboodle.start().await;
```

This port number needs to be the same for all of the peers that want to discover each other; disjoint applications using Kaboodle can run independently on the same network so long as they use distinct port numbers.

At any point, you can retrieve the mesh's fingerprint and a list of all known peers:

```rust
let fingerprint = kaboodle.get_fingerprint().await;
let peers = kaboodle.get_peers().await;
println!("{fingerprint} {peers:?}");
```

The fingerprint is an SHA-256 hash of the membership of the mesh from the current instance's perspective; all machines in the mesh should converge on the same hash value fairly rapidly. The list of peers is a `Vec<SocketAddr>` giving the IP address and UDP port number on which the given peer is listening for messages. Note that this is distinct from the broadcast port number: all peers listen on the broadcast port, but they also send one-to-one messages to each other on a separate port.

## Caveats & known issues

This crate is still at an early stage of active development and is likely not yet fit for production usage. Pull requests, feature requests, and feedback are all very much welcome.

At this point, Kaboodle only supports IPv4. This is for reasons of expedience and not due to policy or technical considerations; stay tuned for IPv6 support in the near future.

## Development

This project uses [just](https://github.com/casey/just) (`brew install just`) for development workflows and automation. Run `just` with no arguments to see a list of available commands.

`cargo run` will run a demo application that looks for peers via port 7475. If you have [zellij](https://zellij.dev) (`brew install zellij`) installed, you can run `just run2x2` to run four copies of the demo app in a 2x2 grid. Try quitting some of the instances and see how the remaining instances discover their absence; each pane has a title above it showing that instance's IP address and port number, as well as the mesh fingerprint from the perspective of that node.
