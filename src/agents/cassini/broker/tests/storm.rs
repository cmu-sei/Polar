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

//! The speed gate's second criterion (docs/architecture/key-custody.md, step 6): a broker taking C new connections at
//! once, each a full TLS 1.3 handshake, completes them at no less than 90% of its rate with a key file when its key is
//! the custody daemon's. The clients' keys are files in both, each connection a fresh configuration so nothing resumes:
//! what is measured is the broker's daemon, which signs once for every connection in the cluster. Ignored by default;
//! run with `cargo test --release -p cassini-broker --test storm -- --ignored --nocapture`.

mod support;
use polar_custody::custody::Custody;
use polar_custody::server::{Callers, bind, serve};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::os::unix::fs::MetadataExt as _;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use support::{Pki, TestBroker, serial};

/// How many connections at once.
const STORM: [usize; 4] = [1, 16, 64, 256];
/// Rounds of each, the rate the median of them.
const ROUNDS: usize = 5;
/// The least the daemon's rate may be, as a share of the key file's.
const LEAST_SHARE: f64 = 0.9;

/// A daemon trusting `pki`'s CA, on a runtime of its own as the daemon's process has (`#[tokio::main]`).
fn daemon_apart(pki: &Pki, dir: &std::path::Path) -> std::path::PathBuf {
    let socket = dir.join("custody.sock");
    let uid = std::fs::metadata(dir).unwrap().uid();
    let ca = std::fs::read_to_string(pki.ca()).unwrap();
    let (ready, bound) = std::sync::mpsc::channel();
    let path = socket.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let listener = bind(&path).unwrap();
                ready.send(()).unwrap();
                let _ = serve(
                    listener,
                    Arc::new(RwLock::new(Custody::new(&ca).unwrap())),
                    Arc::new(Callers {
                        installer_uid: uid,
                        workload_uids: vec![uid],
                    }),
                )
                .await;
            });
    });
    bound.recv().unwrap();
    socket
}

/// A client configuration of its own for one connection, its key a file: nothing to resume from.
fn fresh_client(pki: &Pki) -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_file(pki.ca()).unwrap())
        .unwrap();
    let (cert, key) = pki.client("storm.test");
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![CertificateDer::from_pem_file(cert).unwrap()],
            PrivateKeyDer::from_pem_file(key).unwrap(),
        )
        .unwrap()
}

/// `count` handshakes with the broker at `addr` started at once: each one's time to a finished handshake, and the
/// wall time of them all.
async fn storm(pki: &Pki, addr: &str, count: usize) -> (Vec<Duration>, Duration) {
    let configs: Vec<_> = (0..count).map(|_| Arc::new(fresh_client(pki))).collect();
    let barrier = Arc::new(tokio::sync::Barrier::new(count));
    let started = Instant::now();
    let tasks: Vec<_> = configs
        .into_iter()
        .map(|config| {
            let (barrier, addr) = (barrier.clone(), addr.to_string());
            tokio::spawn(async move {
                barrier.wait().await;
                let began = Instant::now();
                let tcp = tokio::net::TcpStream::connect(&addr).await.unwrap();
                let tls = tokio_rustls::TlsConnector::from(config)
                    .connect(ServerName::try_from("localhost").unwrap(), tcp)
                    .await
                    .expect("a handshake with the broker");
                let took = began.elapsed();
                drop(tls);
                took
            })
        })
        .collect();
    let mut times = Vec::new();
    for task in tasks {
        times.push(task.await.unwrap());
    }
    (times, started.elapsed())
}

/// For each storm size: the median rate over the rounds, and the p50 and p99 of every handshake.
async fn measure(pki: &Pki, addr: &str) -> Vec<(usize, f64, Duration, Duration)> {
    let mut rows = Vec::new();
    for count in STORM {
        let mut rates = Vec::new();
        let mut all = Vec::new();
        for _ in 0..ROUNDS {
            let (times, wall) = storm(pki, addr, count).await;
            rates.push(count as f64 / wall.as_secs_f64());
            all.extend(times);
        }
        rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        all.sort();
        let at = |p: f64| all[((all.len() as f64 - 1.0) * p) as usize];
        rows.push((count, rates[ROUNDS / 2], at(0.5), at(0.99)));
    }
    rows
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_broker_whose_key_is_the_daemon_s_keeps_its_handshake_rate_in_a_storm() {
    let _only = serial().await;

    let files = TestBroker::start_on(Pki::new()).await;
    let with_files = measure(&files.pki, &files.addr).await;
    files.stop().await;

    let dir = tempfile::tempdir().unwrap();
    let mut pki = Pki::new();
    let socket = daemon_apart(&pki, dir.path());
    tokio::task::block_in_place(|| pki.use_custody(&socket));
    let custody = TestBroker::start_on(pki).await;
    let with_daemon = measure(&custody.pki, &custody.addr).await;
    custody.stop().await;

    println!("at once   files /s   p50 / p99             daemon /s   p50 / p99             share");
    let mut missed = Vec::new();
    for ((count, f_rate, f50, f99), (_, d_rate, d50, d99)) in with_files.iter().zip(&with_daemon) {
        let share = d_rate / f_rate;
        println!(
            "{count:>7}   {f_rate:>8.0}   {f50:>8.1?} / {f99:>8.1?}   {d_rate:>8.0}   {d50:>8.1?} / {d99:>8.1?}   {:>5.1}%",
            share * 100.0
        );
        if share < LEAST_SHARE {
            missed.push(format!("{count} at once: {:.1}%", share * 100.0));
        }
    }
    assert!(
        missed.is_empty(),
        "the gate's second criterion, at least {:.0}% of the key file's rate, is missed: {}",
        LEAST_SHARE * 100.0,
        missed.join("; ")
    );
}
