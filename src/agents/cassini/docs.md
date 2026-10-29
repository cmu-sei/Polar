<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

# Cassini Client

The cassini client libary. Contains an actor implementation for a simple TCP Client that can connect and interact with the message broker.
Its variables are `BROKER_ADDR`, `CASSINI_SERVER_NAME`, `TLS_CA_CERT`, `TLS_CLIENT_CERT`, and `TLS_CLIENT_KEY` or
`POLAR_CUSTODY_SOCKET` (`client/src/environment.rs`). `cassini-client`, its command-line form, publishes and asks the
broker about itself; how to run it against a broker of your own is in `broker/readme.md`, "Running it on its own".

# Cassini load harness

*Brought up to date 2026-10-28.* The harness described here before (a Dhall test plan, supervised producer and
consumer actors, and a test control plane run with `cargo run -- --config config.dhall`) was never finished and is gone
(`docs/architecture/cassini_testing.md`, superseded 2026-10-21). The load harness is now two builder agents run as
Kubernetes Jobs by `infra/load-harness.nu`: `test/README.md` describes them, and `docs/architecture/broker-baseline.md`
has how to run it and what it has measured.
