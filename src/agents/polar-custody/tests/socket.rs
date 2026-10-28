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

//! The daemon through its socket: the whole flow polar-init and the workload go through, who may connect, many
//! signatures at once, and a real TLS 1.3 handshake signed by it.

mod common;

use common::*;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use polar_custody::client::{ClientError, CustodyClient, CustodyClientCert, certified_key};
use polar_custody::protocol::{Refusal, Slot};
use polar_custody::server::Callers;
use polar_custody::tls::{CLIENT_CONTEXT, certificate_verify_input};
use std::sync::Arc;
use x509_parser::prelude::*;

fn verifying_key_of(certificate_pem: &str) -> VerifyingKey {
    let (_, block) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes()).unwrap();
    let (_, cert) = X509Certificate::from_der(&block.contents).unwrap();
    VerifyingKey::from_sec1_bytes(&cert.public_key().subject_public_key.data).unwrap()
}

/// polar-init's part: a CSR, certified, installed. The certificate.
fn installed(client: &CustodyClient, ca: &TestCa) -> String {
    let csr = client.make_csr(Slot::Client, IDENTITY, &[]).unwrap();
    let certificate = certify(ca, &csr);
    client
        .install(Slot::Client, vec![certificate.clone()])
        .unwrap();
    certificate
}

#[tokio::test(flavor = "multi_thread")]
async fn polar_init_installs_and_the_workload_signs_through_the_socket() {
    let ca = test_ca();
    let daemon = daemon(&ca, both()).await;
    let socket = daemon.socket.clone();
    tokio::task::spawn_blocking(move || {
        let client = CustodyClient::new(socket);
        let certificate = installed(&client, &ca);
        assert_eq!(
            client.certificate(Slot::Client).unwrap(),
            vec![certificate.clone()]
        );
        let message = certificate_verify_input(CLIENT_CONTEXT, &[5; 32]);
        let signature = Signature::from_der(&client.sign(Slot::Client, &message).unwrap()).unwrap();
        assert!(
            verifying_key_of(&certificate)
                .verify(&message, &signature)
                .is_ok()
        );
        assert!(matches!(
            client.sign(Slot::Client, b"a document in the agent's name"),
            Err(ClientError::Refused(Refusal::NotTls13Handshake))
        ));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_that_is_neither_the_installer_nor_the_workload_is_turned_away() {
    let ca = test_ca();
    let daemon = daemon(
        &ca,
        Callers {
            installer_uid: own_uid().wrapping_add(1),
            workload_uids: vec![own_uid().wrapping_add(2)],
        },
    )
    .await;
    let socket = daemon.socket.clone();
    tokio::task::spawn_blocking(move || {
        let client = CustodyClient::new(socket);
        assert!(matches!(
            client.make_csr(Slot::Client, IDENTITY, &[]),
            Err(ClientError::Frame(_))
        ));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_installer_alone_may_not_sign_and_the_workload_alone_may_not_install() {
    let ca = test_ca();
    let installer_only = daemon(
        &ca,
        Callers {
            installer_uid: own_uid(),
            workload_uids: vec![],
        },
    )
    .await;
    let socket = installer_only.socket.clone();
    let ca_pem_for_workload = test_ca();
    tokio::task::spawn_blocking(move || {
        let client = CustodyClient::new(socket);
        installed(&client, &ca);
        let message = certificate_verify_input(CLIENT_CONTEXT, &[5; 32]);
        assert!(matches!(
            client.sign(Slot::Client, &message),
            Err(ClientError::Refused(Refusal::NotPermitted))
        ));
    })
    .await
    .unwrap();
    let workload_only = daemon(
        &ca_pem_for_workload,
        Callers {
            installer_uid: own_uid().wrapping_add(1),
            workload_uids: vec![own_uid()],
        },
    )
    .await;
    let socket = workload_only.socket.clone();
    tokio::task::spawn_blocking(move || {
        let client = CustodyClient::new(socket);
        assert!(matches!(
            client.make_csr(Slot::Client, IDENTITY, &[]),
            Err(ClientError::Refused(Refusal::NotPermitted))
        ));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn many_signatures_at_once_each_answered_and_each_good() {
    let ca = test_ca();
    let daemon = daemon(&ca, both()).await;
    let client = Arc::new(CustodyClient::new(daemon.socket.clone()));
    let setup = client.clone();
    let certificate = tokio::task::spawn_blocking(move || installed(&setup, &ca))
        .await
        .unwrap();
    let verifying = verifying_key_of(&certificate);
    let mut workers = Vec::new();
    for worker in 0..16u8 {
        let client = client.clone();
        workers.push(tokio::task::spawn_blocking(move || {
            (0..25u8)
                .map(|i| {
                    let message = certificate_verify_input(
                        CLIENT_CONTEXT,
                        &[worker.wrapping_mul(31).wrapping_add(i); 32],
                    );
                    (
                        message.clone(),
                        client.sign(Slot::Client, &message).unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        }));
    }
    let mut signed = 0;
    for worker in workers {
        for (message, der) in worker.await.unwrap() {
            assert!(
                verifying
                    .verify(&message, &Signature::from_der(&der).unwrap())
                    .is_ok()
            );
            signed += 1;
        }
    }
    assert_eq!(signed, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tls13_handshake_with_client_authentication_is_signed_by_the_daemon() {
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let ca = test_ca();
    let daemon = daemon(&ca, both()).await;
    let client = Arc::new(CustodyClient::new(daemon.socket.clone()));
    let setup = client.clone();
    let ca_pem = ca.pem.clone();
    // The server: its own key in a file, as Cassini's is today, demanding a client certificate from the same CA.
    let server_key = rcgen::KeyPair::generate().unwrap();
    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    server_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = server_params.signed_by(&server_key, &ca.issuer).unwrap();
    let certified = tokio::task::spawn_blocking(move || {
        installed(&setup, &ca);
        certified_key(setup, Slot::Client).unwrap()
    })
    .await
    .unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
        .unwrap();
    let roots = Arc::new(roots);
    let server_config =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_client_cert_verifier(
                rustls::server::WebPkiClientVerifier::builder(roots.clone())
                    .build()
                    .unwrap(),
            )
            .with_single_cert(
                vec![server_cert.der().clone()],
                PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes()).unwrap(),
            )
            .unwrap();
    let client_config =
        rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_root_certificates(roots)
            .with_client_cert_resolver(Arc::new(CustodyClientCert(Arc::new(certified))));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
    let served = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(tcp).await.unwrap();
        let (_, connection) = tls.get_ref();
        let peer = connection.peer_certificates().unwrap()[0].clone();
        let (_, cert) = X509Certificate::from_der(peer.as_ref()).unwrap();
        cert.subject_alternative_name()
            .unwrap()
            .unwrap()
            .value
            .general_names
            .iter()
            .find_map(|n| match n {
                GeneralName::DNSName(d) => Some(d.to_string()),
                _ => None,
            })
            .unwrap()
    });
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));
    let tcp = tokio::net::TcpStream::connect(address).await.unwrap();
    connector
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .expect("the handshake, signed through the daemon");
    assert_eq!(
        served.await.unwrap(),
        IDENTITY,
        "the server saw the daemon's certificate"
    );
}

/// The custodian's part, through the socket (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"):
/// polar-init installs the custodian's certificate and gives the plan, asks until it is ready; the workload unwraps
/// through the escrow key the custodian holds, and never signs with the custodian's key.
#[tokio::test(flavor = "multi_thread")]
async fn polar_init_gives_the_plan_waits_for_ready_and_the_workload_unwraps_through_the_escrow_key()
{
    use cert_issuer_common::custody::{CustodyPermission, custodian_identity};
    let ca = test_ca();
    let daemon = daemon(&ca, both()).await;
    let socket = daemon.socket.clone();
    let custody = daemon.custody.clone();
    let key = polar_custody::escrow::EscrowKey::generate().unwrap();
    let public = key.public_key_pem().unwrap();
    tokio::task::spawn_blocking(move || {
        let client = CustodyClient::new(socket);
        let csr = client
            .make_csr(Slot::Custodian, &custodian_identity(IDENTITY), &[])
            .unwrap();
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        request.params.subject_alt_names.push(rcgen::SanType::URI(
            CustodyPermission::Escrow
                .to_san_string()
                .try_into()
                .unwrap(),
        ));
        request.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let certificate = request.signed_by(&ca.issuer).unwrap().pem();
        client.install(Slot::Custodian, vec![certificate]).unwrap();
        client.bootstrap(IDENTITY, false, None, true).unwrap();
        assert!(!client.ready().unwrap(), "nothing fetched yet");
        // What the custodian does: the escrow key retrieved into it.
        {
            let mut custody = custody.write().unwrap();
            let pem = as_a_retrieval_opens_it(&key);
            custody.hold_retrieved_escrow(&pem).unwrap();
            custody.set_ready();
        }
        assert!(client.ready().unwrap());
        let secret = polar_escrow::EscrowedSecret::encrypt(b"a token", &public).unwrap();
        let unwrapped = client.unwrap(secret.wrapped_key()).unwrap();
        let opened = secret
            .decrypt_with(polar_escrow::Suite::HpkeP256, |_| Ok(unwrapped.to_vec()))
            .unwrap();
        assert_eq!(opened.as_slice(), b"a token");
        assert!(matches!(
            client.sign(
                Slot::Custodian,
                &certificate_verify_input(CLIENT_CONTEXT, &[5; 32])
            ),
            Err(ClientError::Refused(Refusal::NotPermitted))
        ));
    })
    .await
    .unwrap();
}

/// The escrow key as a retrieval opens it: its PKCS#8 PEM.
fn as_a_retrieval_opens_it(key: &polar_custody::escrow::EscrowKey) -> String {
    // Sealed for escrow and opened again, as the credentials agent and the custodian do.
    let recipient = rsa::RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
    let sealed = key.sealed_to(&recipient.to_public_key()).unwrap();
    String::from_utf8(sealed.decrypt(&recipient).unwrap().to_vec()).unwrap()
}
