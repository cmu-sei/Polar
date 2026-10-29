/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

#![allow(clippy::incompatible_msrv)]
use cassini_broker::agent::CassiniBroker;
use std::process::ExitCode;

// The one `main` every agent has: it answers `--help`, `--scaffold` and `--check`, judges the
// configuration, and starts the broker. A broker that ends has failed, so it exits 1 whether it
// returned an error or not; the Job starts a fresh pod, which gets fresh certificates.
fn main() -> ExitCode {
    polar_agent_builder::run::<CassiniBroker>()
}

// Every Polar binary allocates with mimalloc (docs/architecture/broker-baseline.md, "musl and the allocators"); the
// broker's other allocators are for the experiment, one at a time (Cargo.toml's features).
#[cfg(any(
    all(feature = "alloc-mimalloc", feature = "alloc-scudo"),
    all(feature = "alloc-mimalloc", feature = "alloc-smalloc"),
    all(feature = "alloc-scudo", feature = "alloc-smalloc"),
))]
compile_error!("choose one allocator feature; the others need --no-default-features");

#[cfg(feature = "alloc-mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "alloc-scudo")]
#[global_allocator]
static GLOBAL: polar_scudo::Scudo = polar_scudo::Scudo;

#[cfg(feature = "alloc-smalloc")]
#[global_allocator]
static GLOBAL: smalloc::Smalloc = smalloc::Smalloc::new();
