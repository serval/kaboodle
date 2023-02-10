# gossip

This repository contains a rough implementation of the [SWIM membership gossip protocol](http://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf), give or take some details.

The goal is to eventually create a generic crate that can be used to discover a set of peers for any arbitrary purpose. For now, it's just a hacky proof of concept demo.

# usage

- `brew install just zellij`
- `just run` will run four copies of the demo in a 2x2 grid. Try quitting some of the instances and see how the remaining instances discover their absence.
