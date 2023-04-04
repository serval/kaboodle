# Changelog

## [0.1.4] - 2023-04-04

- peer identities can now be changed after instantation, and identity changes will after the mesh fingerprint. (For now, you can only change the identity while Kaboodle is stopped, though this should change in the future.)
- Kaboodle will no longer send messages that are too big for its peers to successfully receive
- `Kaboodle::discover_mesh_member` allows you to a member of the mesh without having to join the mesh
- improve how we read incoming packets to avoid unnecessary delays; this improved latency for `discover_mesh_member` substantially
