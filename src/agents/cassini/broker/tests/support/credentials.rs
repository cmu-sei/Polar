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

//! The credentials agent, as the broker and the custody daemon hear it, for the tests of revocation and of watchers
//! (docs/architecture/key-custody.md, step 5). Apart from `support`, which the cert-issuer server's tests share.

use crate::support::{TestBroker, TestClient};

/// The identity the broker believes authority from, by default.
pub const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";

/// The credentials agent as the broker and the custody daemon hear it: its own identity, publishing what it signs on
/// the authority topics.
pub struct CredentialsAgent {
    pub client: TestClient,
    signer: rsa::RsaPrivateKey,
    version: std::sync::atomic::AtomicU64,
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
                "g:cassini-client".to_string(),
            ],
        );
        // One key for the whole test binary: each broker pins it afresh, and an RSA key is slow to make.
        static SIGNER: std::sync::OnceLock<rsa::RsaPrivateKey> = std::sync::OnceLock::new();
        Self {
            client: TestClient::connect(broker, CREDENTIALS).await,
            signer: SIGNER
                .get_or_init(|| rsa::RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap())
                .clone(),
            version: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Publishes, as a snapshot signed with its key, that `serials` are every certificate revoked.
    pub fn revoke(&self, serials: &[String]) {
        self.client.publish(
            credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
            &self.revoked_snapshot(serials, &self.signer),
        );
    }

    /// A snapshot saying `serials` are revoked, signed by `signer` and carrying its key.
    pub fn revoked_snapshot(&self, serials: &[String], signer: &rsa::RsaPrivateKey) -> Vec<u8> {
        use credentials_agent_common::authority_update::{
            AuthorityBody, AuthorityPayload, AuthoritySnapshot, sign,
        };
        use rsa::pkcs8::EncodePublicKey;
        let version = self
            .version
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let revoked = AuthorityPayload {
            sequence: version,
            version,
            body: AuthorityBody::Revoked {
                serials: serials.to_vec(),
            },
            authored_by: None,
        };
        let snapshot = AuthoritySnapshot {
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
