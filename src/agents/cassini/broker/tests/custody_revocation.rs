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

//! A revoked pod's custody daemon destroys its keys, and the broker ends its sessions (docs/architecture/key-custody.md,
//! step 5), end to end: a broker, a daemon serving the pod's workload on its own thread and runtime, as it is a process
//! of its own in a pod, the daemon's custodian session ("Part A's bootstrap: the custodian"), and the credentials agent
//! publishing what it signs. And the custodian's bootstrap through the broker: the workload's certificate asked for
//! and installed, the broker's floor admitting a custodian's requests.

#[path = "support/credentials.rs"]
mod credentials;
mod support;

use cassini_client::{ClientKey, TCPClientConfig};
use credentials::{CREDENTIALS, CredentialsAgent};
use polar_custody::client::{ClientError, CustodyClient};
use polar_custody::custody::Custody;
use polar_custody::protocol::{Refusal, Slot};
use polar_custody::server::{Callers, bind, serve};
use polar_custody_daemon::watch::{Watcher, WatcherArgs};
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use support::{TestBroker, TestClient, serial};

const POD: &str = "git-repo-observer.polar.serviceaccount.cluster.local";
const CUSTODIAN: &str = "custody.git-repo-observer.polar.serviceaccount.cluster.local";

/// A pod: its daemon, serving the custody on a thread and runtime of its own, its client slot holding a certificate
/// as polar-init leaves it, and the certificate written where the workload reads it.
struct Pod {
    _dir: tempfile::TempDir,
    custody: Arc<RwLock<Custody>>,
    socket: PathBuf,
    certificate: PathBuf,
    serial: String,
    /// The custodian's own certificate's serial.
    custodian_serial: String,
}

async fn pod(broker: &TestBroker) -> Pod {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("custody.sock");
    let uid = std::fs::metadata(dir.path()).unwrap().uid();
    let ca = std::fs::read_to_string(broker.pki.ca()).unwrap();
    let custody = Arc::new(RwLock::new(Custody::new(&ca).unwrap()));
    let (ready, bound) = std::sync::mpsc::channel();
    let (path, served) = (socket.clone(), custody.clone());
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
                    served,
                    Arc::new(Callers {
                        installer_uid: uid,
                        workload_uids: vec![uid],
                    }),
                )
                .await;
            });
    });
    bound.recv().unwrap();
    // polar-init's part: the custodian's certificate, then (as a bootstrap service's polar-init does) the workload's,
    // and a plan with nothing for the custodian to fetch.
    let daemon = CustodyClient::new(&socket);
    let custodian = custodian(broker, &daemon, &[]);
    let csr = tokio::task::block_in_place(|| daemon.make_csr(Slot::Client, POD, &[])).unwrap();
    let issued = broker.pki.certify_client_csr(&csr);
    tokio::task::block_in_place(|| daemon.install(Slot::Client, vec![issued.clone()])).unwrap();
    tokio::task::block_in_place(|| daemon.bootstrap(POD, false, None, false)).unwrap();
    let certificate = dir.path().join("cert.pem");
    std::fs::write(&certificate, &issued).unwrap();
    let serial = cert_issuer_common::serial::serial_from_pem(&issued).unwrap();
    Pod {
        _dir: dir,
        custody,
        socket,
        certificate,
        serial,
        custodian_serial: cert_issuer_common::serial::serial_from_pem(&custodian).unwrap(),
    }
}

/// The custodian's certificate, made in the daemon and signed by the broker's CA with `permissions`, installed.
fn custodian(
    broker: &TestBroker,
    daemon: &CustodyClient,
    permissions: &[cert_issuer_common::custody::CustodyPermission],
) -> String {
    let csr =
        tokio::task::block_in_place(|| daemon.make_csr(Slot::Custodian, CUSTODIAN, &[])).unwrap();
    let names: Vec<String> = permissions.iter().map(|p| p.to_san_string()).collect();
    let issued = broker.pki.certify_client_csr_carrying(&csr, &names);
    tokio::task::block_in_place(|| daemon.install(Slot::Custodian, vec![issued.clone()])).unwrap();
    issued
}

/// The pod's workload, its Cassini client signing through the daemon.
async fn workload(broker: &TestBroker, pod: &Pod) -> TestClient {
    let mut config = TCPClientConfig::from_values(
        broker.addr.clone(),
        "localhost".to_string(),
        broker.pki.ca().to_string_lossy().into_owned(),
        pod.certificate.to_string_lossy().into_owned(),
        String::new(),
    );
    config.client_key = ClientKey::Custody(pod.socket.display().to_string());
    TestClient::connect_configured(config).await
}

/// The daemon's watcher, as the daemon starts it; what it is told once revoked comes back on the channel.
async fn watch(broker: &TestBroker, pod: &Pod) -> tokio::sync::mpsc::UnboundedReceiver<Vec<Slot>> {
    let (revoked, told) = tokio::sync::mpsc::unbounded_channel();
    ractor::Actor::spawn(
        None,
        Watcher,
        WatcherArgs {
            custody: pod.custody.clone(),
            ca_pem: std::fs::read_to_string(broker.pki.ca()).unwrap(),
            broker: broker.addr.clone(),
            server_name: "localhost".to_string(),
            publisher: CREDENTIALS.to_string(),
            on_revoked: Arc::new(move |slots| {
                let _ = revoked.send(slots);
            }),
        },
    )
    .await
    .unwrap();
    told
}

async fn told_revoked(told: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<Slot>>) -> Vec<Slot> {
    tokio::time::timeout(Duration::from_secs(20), told.recv())
        .await
        .expect("the daemon is told in time")
        .expect("told")
}

fn the_daemon_refuses_to_sign(pod: &Pod) -> bool {
    let daemon = CustodyClient::new(&pod.socket);
    let message = {
        let mut m = vec![0x20; 64];
        m.extend_from_slice(b"TLS 1.3, client CertificateVerify\0");
        m.extend_from_slice(&[9; 32]);
        m
    };
    matches!(
        tokio::task::block_in_place(|| daemon.sign(Slot::Client, &message)),
        Err(ClientError::Refused(Refusal::Revoked))
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_pod_s_daemon_destroys_its_keys_and_its_workload_s_session_ends() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    credentials.revoke(&[]);
    credentials.answer_requests().await;
    let pod = pod(&broker).await;
    let workload = workload(&broker, &pod).await;
    let mut told = watch(&broker, &pod).await;
    // The watcher is up once it has asked for the current authority.
    credentials
        .client
        .wait_for("the watcher's request", Duration::from_secs(20), |_| {
            credentials.requests_from(CUSTODIAN) >= 1
        })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!pod.custody.read().unwrap().is_revoked());

    credentials.revoke(std::slice::from_ref(&pod.serial));
    assert_eq!(told_revoked(&mut told).await, vec![Slot::Client]);
    assert!(the_daemon_refuses_to_sign(&pod), "its keys are gone");
    workload
        .wait_for(
            "the workload's connection closed",
            Duration::from_secs(10),
            |c| c.lost_its_connection(),
        )
        .await;
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revocation_published_while_the_daemon_was_away_is_heard_when_it_comes() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    let pod = pod(&broker).await;
    credentials.revoke(std::slice::from_ref(&pod.serial));
    credentials.answer_requests().await;
    let mut told = watch(&broker, &pod).await;
    assert_eq!(told_revoked(&mut told).await, vec![Slot::Client]);
    assert!(the_daemon_refuses_to_sign(&pod));
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_signs_for_its_workload_before_it_has_heard_any_authority() {
    let _serial = serial().await;
    // No credentials agent at all, as when its own pod starts.
    let broker = TestBroker::start().await;
    let pod = pod(&broker).await;
    let _told = watch(&broker, &pod).await;
    let workload = workload(&broker, &pod).await;
    assert_eq!(workload.registration_events(), 1);
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workload_and_its_daemon_s_watcher_stand_side_by_side() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    credentials.revoke(&[]);
    credentials.answer_requests().await;
    let pod = pod(&broker).await;
    let workload = workload(&broker, &pod).await;
    let _told = watch(&broker, &pod).await;
    credentials
        .client
        .wait_for("the watcher's request", Duration::from_secs(20), |_| {
            credentials.requests_from(CUSTODIAN) >= 1
        })
        .await;
    // Long enough for either to have closed the other more than once, were they two sessions of one identity.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !workload.lost_its_connection(),
        "the custodian's session does not supersede its workload's"
    );
    assert_eq!(
        credentials.requests_from(CUSTODIAN),
        1,
        "nor the workload's the daemon's, which would ask again on each registration"
    );
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_watches_again_once_cassini_is_back() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    credentials.revoke(&[]);
    credentials.answer_requests().await;
    let pod = pod(&broker).await;
    let mut told = watch(&broker, &pod).await;
    credentials
        .client
        .wait_for("the watcher's request", Duration::from_secs(20), |_| {
            credentials.requests_from(CUSTODIAN) >= 1
        })
        .await;

    broker.restart(Duration::from_secs(2)).await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    credentials.revoke(&[]);
    credentials.answer_requests().await;
    credentials
        .client
        .wait_for(
            "the watcher's request, from its new connection",
            Duration::from_secs(40),
            |_| credentials.requests_from(CUSTODIAN) >= 1,
        )
        .await;
    credentials.revoke(std::slice::from_ref(&pod.serial));
    assert_eq!(told_revoked(&mut told).await, vec![Slot::Client]);
    broker.stop().await;
}

/// The custodian is its own identity: however many sessions a compromised workload opens under its certificate, each
/// superseding the last, none touches the custodian's, which hears the revocation.
#[tokio::test(flavor = "multi_thread")]
async fn sessions_the_workload_opens_do_not_keep_its_custodian_from_its_revocation() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    credentials.revoke(&[]);
    credentials.answer_requests().await;
    let pod = pod(&broker).await;
    let mut told = watch(&broker, &pod).await;
    let mut sessions = Vec::new();
    for _ in 0..3 {
        sessions.push(workload(&broker, &pod).await);
    }
    credentials.revoke(std::slice::from_ref(&pod.serial));
    assert_eq!(told_revoked(&mut told).await, vec![Slot::Client]);
    assert!(the_daemon_refuses_to_sign(&pod));
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_custodian_s_own_certificate_revoked_destroys_every_key() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    credentials.revoke(&[]);
    credentials.answer_requests().await;
    let pod = pod(&broker).await;
    let mut told = watch(&broker, &pod).await;
    credentials
        .client
        .wait_for("the custodian's request", Duration::from_secs(20), |_| {
            credentials.requests_from(CUSTODIAN) >= 1
        })
        .await;
    credentials.revoke(std::slice::from_ref(&pod.custodian_serial));
    assert_eq!(told_revoked(&mut told).await, vec![Slot::Custodian]);
    assert!(
        the_daemon_refuses_to_sign(&pod),
        "the workload's keys go with the custodian's"
    );
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custodian_the_broker_refuses_as_revoked_destroys_every_key() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    let pod = pod(&broker).await;
    // The broker learns the custodian is revoked before the custodian connects; the credentials agent then answers
    // nobody, so the only word the custodian has is the broker's refusal.
    credentials.revoke(std::slice::from_ref(&pod.custodian_serial));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut told = watch(&broker, &pod).await;
    let slots = told_revoked(&mut told).await;
    assert!(slots.contains(&Slot::Custodian), "{slots:?}");
    assert!(the_daemon_refuses_to_sign(&pod));
    broker.stop().await;
}

/// The broker refuses the certificate the custodian connects with, its own, so that is the one the daemon reports
/// revoked, as when a set it believes names it: its workload's certificate was not revoked. Every key goes all the same.
/// (The custodian's own revocation reaches it either way: the broker hears the set as it passes and closes the
/// custodian's connection, perhaps before the set is delivered on it, and then refuses it at its next registration.)
#[tokio::test(flavor = "multi_thread")]
async fn a_custodian_refused_as_revoked_reports_only_its_own_certificate_revoked() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    let pod = pod(&broker).await;
    // As above: the broker's refusal is the only word the custodian has.
    credentials.revoke(std::slice::from_ref(&pod.custodian_serial));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut told = watch(&broker, &pod).await;
    assert_eq!(
        told_revoked(&mut told).await,
        vec![Slot::Custodian],
        "the workload's certificate, which it also held, was not revoked"
    );
    assert!(the_daemon_refuses_to_sign(&pod), "every key goes");
    broker.stop().await;
}

/// The custodian's bootstrap through a broker enforcing its floor, with no policy (docs/architecture/key-custody.md,
/// "Part A's bootstrap: the custodian"): the custodian asks for its workload's client certificate on its own session,
/// the credentials agent answers, and the certificate is installed for the workload to sign with.
#[tokio::test(flavor = "multi_thread")]
async fn the_custodian_asks_for_its_workload_s_certificate_and_installs_it_through_an_enforcing_broker()
 {
    use cert_issuer_common::custody::CustodyPermission;
    use credentials_agent_common::custodian::CustodianIssuanceRequest;
    use credentials_agent_common::issuance::IssuanceReply;
    use credentials_agent_common::{
        CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC, CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC,
    };
    use polar::request_reply::Envelope;
    let _serial = serial().await;
    let broker = TestBroker::start_enforcing().await;
    let credentials = Arc::new(CredentialsAgent::connect(&broker).await);
    credentials.revoke(&[]);
    credentials.answer_requests().await;
    credentials
        .client
        .subscribe(CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC);
    credentials
        .client
        .wait_for(
            "its subscription to custodians' requests",
            Duration::from_secs(10),
            |c| c.subscribed(CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC),
        )
        .await;
    // A pod whose polar-init has installed only the custodian's certificate, and planned a client certificate.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("custody.sock");
    let uid = std::fs::metadata(dir.path()).unwrap().uid();
    let ca = std::fs::read_to_string(broker.pki.ca()).unwrap();
    let custody = Arc::new(RwLock::new(Custody::new(&ca).unwrap()));
    let (ready, bound) = std::sync::mpsc::channel();
    let (path, served) = (socket.clone(), custody.clone());
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
                    served,
                    Arc::new(Callers {
                        installer_uid: uid,
                        workload_uids: vec![uid],
                    }),
                )
                .await;
            });
    });
    bound.recv().unwrap();
    let daemon = CustodyClient::new(&socket);
    let custodian_certificate = custodian(&broker, &daemon, &[CustodyPermission::Client]);
    tokio::task::block_in_place(|| daemon.bootstrap(POD, true, None, false)).unwrap();
    assert!(!tokio::task::block_in_place(|| daemon.ready()).unwrap());
    let pod = Pod {
        _dir: dir,
        custody: custody.clone(),
        socket: socket.clone(),
        certificate: PathBuf::new(),
        serial: String::new(),
        custodian_serial: cert_issuer_common::serial::serial_from_pem(&custodian_certificate)
            .unwrap(),
    };
    let _told = watch(&broker, &pod).await;
    // The credentials agent's part: the request, from the custodian, signed for the workload.
    credentials
        .client
        .wait_for("the custodian's request", Duration::from_secs(30), |c| {
            !c.received_from(CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC)
                .is_empty()
        })
        .await;
    let (payload, by) = credentials
        .client
        .received_from(CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC)
        .remove(0);
    assert_eq!(
        by.as_deref(),
        Some(CUSTODIAN),
        "asked on the custodian's own session"
    );
    let request =
        rkyv::from_bytes::<Envelope<CustodianIssuanceRequest>, rkyv::rancor::Error>(&payload)
            .unwrap();
    let issued = broker.pki.certify_client_csr(&request.payload.csr_pem);
    let reply = rkyv::to_bytes::<rkyv::rancor::Error>(&Envelope {
        correlation_id: request.correlation_id,
        payload: IssuanceReply::Issued {
            certificate_pem: issued.clone(),
            ca_chain_pem: String::new(),
            escrow_public_key_pem: String::new(),
        },
    })
    .unwrap()
    .to_vec();
    credentials
        .client
        .publish(CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC, &reply);
    // Ready, and the workload signs with the key the custodian holds for it.
    let start = std::time::Instant::now();
    while !tokio::task::block_in_place(|| daemon.ready()).unwrap() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the custodian is never ready"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        tokio::task::block_in_place(|| daemon.certificate(Slot::Client)).unwrap(),
        vec![issued]
    );
    let message = {
        let mut m = vec![0x20; 64];
        m.extend_from_slice(b"TLS 1.3, client CertificateVerify\0");
        m.extend_from_slice(&[9; 32]);
        m
    };
    assert!(tokio::task::block_in_place(|| daemon.sign(Slot::Client, &message)).is_ok());
    broker.stop().await;
}
