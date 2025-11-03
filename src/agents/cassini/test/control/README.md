# Cassini Test Control Plane

An executable control plane that will automatically start services to test the cassini broker.
Eventually, this could auto-provision the necessary infrastructure within desired environments, such as k8s clusters or on-prem bare metal. We could also just extend it to work beyond cassini and extract it from its repo. Time will tell.

**Usage**:

```bash
cargo run -- --config config.dhall
```
