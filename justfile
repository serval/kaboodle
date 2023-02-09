# List available recipes
help:
    just -l

# Run several copies of the app in a 2x2 grid
run:
    cargo build && zellij --layout 2x2-layout.kdl
