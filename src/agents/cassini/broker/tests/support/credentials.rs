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

//! The credentials agent, as the broker and the custody daemon hear it, for the tests of revocation and of custody
//! (docs/architecture/key-custody.md, step 5). Apart from `support`, which the cert-issuer server's tests share.
//! Each test binary that includes it uses only some of it.
#![allow(dead_code)]

use crate::support::{TestBroker, TestClient};

/// The identity the broker believes authority from, by default.
pub const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";

/// The credentials agent as the broker and the custody daemon hear it: its own identity, publishing what it signs on
/// the authority topics.
pub struct CredentialsAgent {
    pub client: TestClient,
    signer: rsa::RsaPrivateKey,
    /// Its signing certificate for `signer`, from the broker's CA, as cert-issuer issues it.
    chain: Vec<String>,
    /// The last snapshot it published: what it answers a request for the current authority with.
    current: std::sync::Mutex<Option<Vec<u8>>>,
}

impl CredentialsAgent {
    /// Connects as the credentials agent, with a signing key of its own.
    pub async fn connect(broker: &TestBroker) -> Self {
        use credentials_agent_common::authority_update::{
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_UPDATES_TOPIC,
        };
        broker.pki.issue_client_carrying(
            CREDENTIALS,
            &[
                format!("p:pub:{AUTHORITY_SNAPSHOT_REPLIES_TOPIC}"),
                format!("p:pub:{AUTHORITY_UPDATES_TOPIC}"),
                format!(
                    "p:sub:{}",
                    credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REQUESTS_TOPIC
                ),
                // A custodian's requests for its workload's certificates, and the answers.
                format!(
                    "p:sub:{}",
                    credentials_agent_common::CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC
                ),
                format!(
                    "p:pub:{}",
                    credentials_agent_common::CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC
                ),
                "g:cassini-client".to_string(),
            ],
        );
        // One key for the whole test binary: each broker pins it afresh, and an RSA key is slow to make.
        static SIGNER: std::sync::OnceLock<rsa::RsaPrivateKey> = std::sync::OnceLock::new();
        let signer = SIGNER
            .get_or_init(|| rsa::RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap())
            .clone();
        let public_key_pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &signer.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        Self {
            client: TestClient::connect(broker, CREDENTIALS).await,
            chain: vec![broker.pki.certify_authority(CREDENTIALS, &public_key_pem)],
            signer,
            current: std::sync::Mutex::new(None),
        }
    }

    /// Publishes, as a snapshot signed with its key, that `serials` are every certificate revoked.
    pub fn revoke(&self, serials: &[String]) {
        let snapshot = self.revoked_snapshot(serials, &self.signer);
        *self.current.lock().unwrap() = Some(snapshot.clone());
        self.client.publish(
            credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
            &snapshot,
        );
    }

    /// From now on, answers each request for the current authority with the last snapshot it published, as the
    /// credentials agent does.
    pub async fn answer_requests(self: &std::sync::Arc<Self>) {
        use credentials_agent_common::authority_update::{
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
        };
        self.client.subscribe(AUTHORITY_SNAPSHOT_REQUESTS_TOPIC);
        self.client
            .wait_for(
                "its subscription to requests",
                std::time::Duration::from_secs(10),
                |c| c.subscribed(AUTHORITY_SNAPSHOT_REQUESTS_TOPIC),
            )
            .await;
        let me = self.clone();
        tokio::spawn(async move {
            let mut answered = 0;
            loop {
                let asked = me.client.received_count(AUTHORITY_SNAPSHOT_REQUESTS_TOPIC);
                if asked > answered {
                    answered = asked;
                    if let Some(current) = me.current.lock().unwrap().clone() {
                        me.client
                            .publish(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, &current);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
    }

    /// How many requests for the current authority it has heard from `identity` (the broker asks too, as no one).
    pub fn requests_from(&self, identity: &str) -> usize {
        self.client
            .received_from(
                credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
            )
            .iter()
            .filter(|(_, by)| by.as_deref() == Some(identity))
            .count()
    }

    /// A snapshot saying `serials` are revoked, signed by `signer` and carrying its key.
    pub fn revoked_snapshot(&self, serials: &[String], signer: &rsa::RsaPrivateKey) -> Vec<u8> {
        use credentials_agent_common::authority_update::{
            AuthorityBody, AuthorityPayload, AuthoritySnapshot, sign,
        };
        use rsa::pkcs8::EncodePublicKey;
        // One count for the whole test binary, as the credentials agent's versions only rise: a stand-in that comes
        // back after a broker's restart is not heard as older.
        static VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let version = VERSION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let revoked = AuthorityPayload {
            sequence: version,
            version,
            body: AuthorityBody::Revoked {
                serials: serials.to_vec(),
                compromised: Vec::new(),
            },
            authored_by: None,
        };
        let snapshot = AuthoritySnapshot {
            certificate_chain_pem: self.chain.clone(),
            public_key_pem: signer
                .to_public_key()
                .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap(),
            updates: vec![sign(&revoked, signer).unwrap()],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&snapshot)
            .unwrap()
            .to_vec()
    }
}
