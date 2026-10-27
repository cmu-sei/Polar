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

//! The broker's listener with the custody daemon's server key (docs/architecture/key-custody.md, step 4): a client
//! connects and registers, the server's handshake signed through the daemon, whose server slot holds the certificate
//! the broker presents.

mod support;
use polar_custody::custody::Custody;
use polar_custody::server::{Callers, bind, serve};
use std::os::unix::fs::MetadataExt as _;
use std::sync::{Arc, RwLock};
use support::{Pki, TestBroker, TestClient, serial};

/// A daemon trusting `pki`'s CA on a thread and runtime of its own, as it is a process of its own in a pod: the
/// listener signs in its handshake on the worker it runs on.
fn daemon_apart(pki: &Pki, dir: &std::path::Path) -> std::path::PathBuf {
    let socket = dir.join("custody.sock");
    let uid = std::fs::metadata(dir).unwrap().uid();
    let ca = std::fs::read_to_string(pki.ca()).unwrap();
    let (ready, bound) = std::sync::mpsc::channel();
    let path = socket.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
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

#[tokio::test(flavor = "multi_thread")]
async fn a_client_registers_with_a_broker_whose_key_is_the_custody_daemon_s() {
    let _only = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let mut pki = Pki::new();
    let socket = daemon_apart(&pki, dir.path());
    tokio::task::block_in_place(|| pki.use_custody(&socket));
    let broker = Arc::new(TestBroker::start_on(pki).await);
    let client = TestClient::connect(&broker, "client.test").await;
    assert_eq!(
        client.registration_events(),
        1,
        "registered over the daemon's handshake"
    );
    broker.stop().await;
}
