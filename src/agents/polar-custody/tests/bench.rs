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

    // In process: the same signature with a key this process holds, through ring, as rustls signs for a workload today.
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
    let random = ring::rand::SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random).unwrap();
    let local =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &random).unwrap();
    let in_process: Vec<Duration> = (0..SIGNATURES)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(local.sign(&random, &message).unwrap());
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

/// The concurrencies the storm asks the daemon for signatures at (docs/architecture/key-custody.md, step 6).
const CONCURRENCY: [usize; 4] = [1, 8, 32, 128];
/// The most the daemon may add to a signature at p99, against one signed in process at the same concurrency.
const MOST_ADDED_AT_P99: Duration = Duration::from_micros(100);
/// Signatures a concurrency asks for, spread over its callers.
const STORM_SIGNATURES: usize = 10_000;

/// `callers` threads at once, each making its share of `STORM_SIGNATURES` with `sign`: every signature's time, and the
/// wall time of the whole.
fn at_once(callers: usize, sign: Arc<dyn Fn() + Send + Sync>) -> (Vec<Duration>, Duration) {
    let barrier = Arc::new(std::sync::Barrier::new(callers));
    let each = STORM_SIGNATURES / callers;
    let started = Instant::now();
    let threads: Vec<_> = (0..callers)
        .map(|_| {
            let (barrier, sign) = (barrier.clone(), sign.clone());
            std::thread::spawn(move || {
                barrier.wait();
                (0..each)
                    .map(|_| {
                        let started = Instant::now();
                        sign();
                        started.elapsed()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let times = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    (times, started.elapsed())
}

/// The gate's first criterion (docs/architecture/key-custody.md, step 6): at each concurrency of the storm, a signature
/// through the daemon costs no more than 100 µs at p99 over one signed in process at the same concurrency. The daemon
/// on a runtime of its own, as its process has; the callers share one client, as one process's handshakes do. Ignored
/// by default; run with `cargo test --release -p polar-custody --test bench -- --ignored --nocapture`.
#[test]
#[ignore]
fn the_daemon_s_cost_at_each_concurrency_of_the_storm() {
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};
    let ca = test_ca();
    let daemon = daemon_apart(&ca, both());
    let client = Arc::new(CustodyClient::new(daemon.socket.clone()));
    let csr = client.make_csr(Slot::Client, IDENTITY, &[]).unwrap();
    client
        .install(Slot::Client, vec![certify(&ca, &csr)])
        .unwrap();
    let message = Arc::new(certificate_verify_input(CLIENT_CONTEXT, &[3; 32]));

    // In process: one key, shared by the threads that sign with it, as rustls shares a workload's.
    let random = ring::rand::SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random).unwrap();
    let local = Arc::new(
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &random).unwrap(),
    );

    println!(
        "callers  in process p50 / p99      daemon p50 / p99        added at p99   daemon signatures/s"
    );
    let mut missed = Vec::new();
    for callers in CONCURRENCY {
        let (key, m, rng) = (local.clone(), message.clone(), random.clone());
        let (in_process, _) = at_once(
            callers,
            Arc::new(move || {
                std::hint::black_box(key.sign(&rng, &m).unwrap());
            }),
        );
        let (c, m) = (client.clone(), message.clone());
        let (through, wall) = at_once(
            callers,
            Arc::new(move || {
                std::hint::black_box(c.sign(Slot::Client, &m).unwrap());
            }),
        );
        let (p50, p99, _) = percentiles(in_process);
        let (d50, d99, _) = percentiles(through.clone());
        let added = d99.saturating_sub(p99);
        println!(
            "{callers:>7}  {p50:>10.1?} / {p99:>10.1?}   {d50:>10.1?} / {d99:>10.1?}   {added:>12.1?}   {:>10.0}",
            through.len() as f64 / wall.as_secs_f64()
        );
        if added > MOST_ADDED_AT_P99 {
            missed.push(format!("{callers} callers: {added:?} added at p99"));
        }
    }
    assert!(
        missed.is_empty(),
        "the gate's first criterion, at most {MOST_ADDED_AT_P99:?} added at p99, is missed: {}",
        missed.join("; ")
    );
}
