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

//! The cost of a signature through the daemon against one in process (docs/architecture/key-custody.md, "Speed").
//! Ignored by default; run with `cargo test --release -p polar-custody --test bench -- --ignored --nocapture`.

mod common;

use common::*;
use polar_custody::client::CustodyClient;
use polar_custody::protocol::Slot;
use polar_custody::tls::{CLIENT_CONTEXT, certificate_verify_input};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SIGNATURES: usize = 5000;

fn percentiles(mut times: Vec<Duration>) -> (Duration, Duration, Duration) {
    times.sort();
    let at = |p: f64| times[((times.len() as f64 - 1.0) * p) as usize];
    (at(0.5), at(0.99), at(1.0))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn a_signature_through_the_socket_against_one_in_process() {
    let ca = test_ca();
    let daemon = daemon(&ca, both()).await;
    let client = Arc::new(CustodyClient::new(daemon.socket.clone()));
    let setup = client.clone();
    tokio::task::spawn_blocking(move || {
        let csr = setup.make_csr(Slot::Client, IDENTITY, &[]).unwrap();
        setup
            .install(Slot::Client, vec![certify(&ca, &csr)])
            .unwrap();
    })
    .await
    .unwrap();
    let message = certificate_verify_input(CLIENT_CONTEXT, &[3; 32]);

    // In process: the same signature, with a key this process holds.
    let local = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
    let in_process: Vec<Duration> = (0..SIGNATURES)
        .map(|_| {
            let started = Instant::now();
            let signature: p256::ecdsa::Signature =
                p256::ecdsa::signature::Signer::sign(&local, &message);
            std::hint::black_box(signature.to_der());
            started.elapsed()
        })
        .collect();

    // Through the socket, one caller.
    let one = client.clone();
    let m = message.clone();
    let socket: Vec<Duration> = tokio::task::spawn_blocking(move || {
        (0..SIGNATURES)
            .map(|_| {
                let started = Instant::now();
                std::hint::black_box(one.sign(Slot::Client, &m).unwrap());
                started.elapsed()
            })
            .collect()
    })
    .await
    .unwrap();

    // Through the socket, eight callers at once, as a reconnection storm would ask.
    let started = Instant::now();
    let mut callers = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        let m = message.clone();
        callers.push(tokio::task::spawn_blocking(move || {
            (0..SIGNATURES / 8)
                .map(|_| {
                    let started = Instant::now();
                    std::hint::black_box(client.sign(Slot::Client, &m).unwrap());
                    started.elapsed()
                })
                .collect::<Vec<_>>()
        }));
    }
    let mut concurrent = Vec::new();
    for caller in callers {
        concurrent.extend(caller.await.unwrap());
    }
    let wall = started.elapsed();

    let (p50, p99, max) = percentiles(in_process);
    println!("in process:          p50 {p50:?}  p99 {p99:?}  max {max:?}");
    let (s50, s99, smax) = percentiles(socket);
    println!(
        "socket, one caller:  p50 {s50:?}  p99 {s99:?}  max {smax:?}  (added at p99: {:?})",
        s99.saturating_sub(p99)
    );
    let (c50, c99, cmax) = percentiles(concurrent);
    println!(
        "socket, 8 callers:   p50 {c50:?}  p99 {c99:?}  max {cmax:?}  ({:.0} signatures/s)",
        SIGNATURES as f64 / wall.as_secs_f64()
    );
}
