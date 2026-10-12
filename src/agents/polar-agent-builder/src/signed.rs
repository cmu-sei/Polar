//! The checks that stand in front of the ConfigurationActor: a signed document is accepted only if it was
//! signed by someone this agent trusts, who may make this change, for this agent.
//! Design: `docs/architecture/signed-configuration.md`.
//!
//! In order, refusing at the first failure:
//!
//! 1. the document has exactly its two fields;
//! 2. the signature chains to the pinned CA, the certificate is valid now and may sign, and the signature
//!    covers the payload ([`polar_signing::verify`]);
//! 3. only now is the payload read, since until the signature held it is not to be trusted at all: it has
//!    the shape an update has;
//! 4. it is addressed to this agent, so a document signed for one agent cannot be applied by another;
//! 5. the signer may make this change ([`polar_signing::Authority`]);
//! 6. the signer is not revoked (a seam only: nothing revokes yet).
//!
//! The version check, and everything about whether the values are acceptable, belong to the
//! ConfigurationActor and run after this. An agent that has no pinned CA to verify against refuses every
//! document: it fails closed.

use cassini_client::TCPClientConfig;
use polar_env::Config;
use polar_scheduler_common::document_type;
use polar_signing::{
    Authority, AuthorityRefusal, Document, LayoutError, RequireCapability, SignerInfo, Trust,
    VerifyError,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Why a document was not accepted. Nothing was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// This agent has no CA to verify against, so it accepts nothing.
    NotConfigured,
    Layout(LayoutError),
    Signature(VerifyError),
    /// Signed and verified, but not an update: the payload does not have the shape of one.
    Payload(String),
    /// A signed document of another kind: a configuration is expected here, not a command or admission.
    WrongType {
        wanted: &'static str,
        got: String,
    },
    /// A command that is not within its time: it has expired, has not started, or claims a window that
    /// is inconsistent or too long. Only commands have one.
    Window(crate::command::WindowError),
    /// Signed for a different agent.
    WrongTarget {
        wanted: String,
        got: String,
    },
    Unauthorized(AuthorityRefusal),
    Revoked(String),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::NotConfigured => {
                write!(
                    f,
                    "this agent has no CA to verify signed configuration against"
                )
            }
            Rejection::Layout(e) => write!(f, "not a signed document: {e}"),
            Rejection::Signature(e) => write!(f, "not signed by anyone this agent trusts: {e}"),
            Rejection::Payload(e) => write!(f, "not an update: {e}"),
            Rejection::WrongType { wanted, got } => {
                write!(f, "a `{got}` document, and a `{wanted}` is expected here")
            }
            Rejection::Window(e) => write!(f, "outside its time: {e}"),
            Rejection::WrongTarget { wanted, got } => {
                write!(f, "signed for `{got}`, and this agent is `{wanted}`")
            }
            Rejection::Unauthorized(e) => write!(f, "signer not authorised: {e}"),
            Rejection::Revoked(who) => write!(f, "the signer's certificate is revoked: {who}"),
        }
    }
}

/// An update that passed every check here. `signer` is who signed it, from their certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub id: String,
    pub version: u64,
    pub changes: Vec<(String, String)>,
    pub signer: SignerInfo,
}

/// What an update says. Strict: a field this does not know is refused, since it would be signed but
/// meaningless.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    /// Checked before this is read; here because the payload is read strictly.
    #[serde(rename = "type")]
    #[allow(dead_code)]
    kind: String,
    id: String,
    target: Target,
    version: u64,
    changes: Vec<Change>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    agent: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    name: String,
    value: String,
}

/// Whether a signer's certificate has been revoked. Nothing revokes certificates yet, so the first
/// implementation says no; this is where a revocation list will be consulted.
pub trait Revocation: Send + Sync {
    fn is_revoked(&self, signer: &SignerInfo) -> bool;
}

pub struct NoRevocation;

impl Revocation for NoRevocation {
    fn is_revoked(&self, _signer: &SignerInfo) -> bool {
        false
    }
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// What every kind of signed document is checked against, shared so that a configuration and a command are
/// held to the same trust, the same clock and the same revocation: the pinned CA, who revokes, which agent
/// this is, and what time it is.
#[derive(Clone)]
pub(crate) struct Common {
    trust: Arc<Trust>,
    revocation: Arc<dyn Revocation>,
    pub(crate) agent: String,
    clock: Clock,
}

impl Common {
    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
    }

    pub(crate) fn set_revocation(&mut self, revocation: Arc<dyn Revocation>) {
        self.revocation = revocation;
    }

    /// The first stage of every document: it has exactly its two fields, and the signature holds (the chain
    /// to the pinned CA, the certificate's validity and use, the signature over the payload). Until it has,
    /// nothing in the payload is to be believed.
    pub(crate) fn signed(&self, document: &Value) -> Result<(Document, SignerInfo), Rejection> {
        let document = Document::from_value(document).map_err(Rejection::Layout)?;
        let signer = polar_signing::verify(&document, &self.trust, self.now())
            .map_err(Rejection::Signature)?;
        Ok((document, signer))
    }

    /// What kind of document this says it is, and a refusal if it is not `wanted`.
    pub(crate) fn expect_kind(
        &self,
        document: &Document,
        wanted: &'static str,
    ) -> Result<(), Rejection> {
        let kind = document
            .payload
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Rejection::Payload("the payload has no text `type`".to_string()))?;
        if kind != wanted {
            return Err(Rejection::WrongType {
                wanted,
                got: kind.to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn check_target(&self, got: String) -> Result<(), Rejection> {
        if got != self.agent {
            return Err(Rejection::WrongTarget {
                wanted: self.agent.clone(),
                got,
            });
        }
        Ok(())
    }

    pub(crate) fn check_not_revoked(&self, signer: &SignerInfo) -> Result<(), Rejection> {
        if self.revocation.is_revoked(signer) {
            return Err(Rejection::Revoked(signer.serial.clone()));
        }
        Ok(())
    }
}

pub struct UpdateVerifier {
    pub(crate) common: Common,
    authority: Arc<dyn Authority>,
}

impl UpdateVerifier {
    /// Verifies for `agent` against `trust`, accepting signers who hold `c:update-configuration`.
    pub fn new(trust: Trust, agent: impl Into<String>) -> Self {
        Self {
            common: Common {
                trust: Arc::new(trust),
                revocation: Arc::new(NoRevocation),
                agent: agent.into(),
                clock: Arc::new(|| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0)
                }),
            },
            authority: Arc::new(RequireCapability::update_configuration()),
        }
    }

    pub fn with_authority(mut self, authority: Arc<dyn Authority>) -> Self {
        self.authority = authority;
        self
    }

    pub fn with_revocation(mut self, revocation: Arc<dyn Revocation>) -> Self {
        self.common.revocation = revocation;
        self
    }

    /// For tests, which check at a time of their own choosing.
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.common.clock = Arc::new(clock);
        self
    }

    /// The verifier an agent gets from its own configuration: it trusts the CA it already pins to talk
    /// to Cassini (`TLS_CA_CERT`), which is the Polar CA. Without a readable CA there is nothing to
    /// verify against, and the agent accepts nothing.
    pub fn from_config(config: &Config, agent: &str) -> Result<Self, String> {
        let settings = TCPClientConfig::from_env(config).map_err(|e| e.to_string())?;
        let path = settings.ca_certificate_path;
        let bundle = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read the CA certificate {path}: {e}"))?;
        let anchors = split_pem_bundle(&bundle);
        if anchors.is_empty() {
            return Err(format!("{path} holds no certificate"));
        }
        let trust = Trust::from_pem(&anchors).map_err(|e| format!("{path}: {e}"))?;
        Ok(Self::new(trust, agent))
    }

    pub fn verify(&self, document: &Value) -> Result<Verified, Rejection> {
        let (document, signer) = self.common.signed(document)?;

        // The signature held, so the payload is what the signer wrote. Now read it: first what kind of
        // document it is, since a command or an admission is not a configuration whatever else it says.
        self.common
            .expect_kind(&document, document_type::CONFIGURATION)?;
        let payload: Payload = serde_json::from_value(document.payload.clone())
            .map_err(|e| Rejection::Payload(e.to_string()))?;
        self.common.check_target(payload.target.agent)?;
        self.authority
            .authorize(&signer, &document.payload)
            .map_err(Rejection::Unauthorized)?;
        self.common.check_not_revoked(&signer)?;
        Ok(Verified {
            id: payload.id,
            version: payload.version,
            changes: payload
                .changes
                .into_iter()
                .map(|c| (c.name, c.value))
                .collect(),
            signer,
        })
    }
}

/// The certificates in a PEM file, each as its own PEM string.
fn split_pem_bundle(bundle: &str) -> Vec<String> {
    const END: &str = "-----END CERTIFICATE-----";
    let mut certs = Vec::new();
    let mut rest = bundle;
    while let Some(end) = rest.find(END) {
        let (one, after) = rest.split_at(end + END.len());
        if one.contains("-----BEGIN CERTIFICATE-----") {
            certs.push(one.trim().to_string());
        }
        rest = after;
    }
    certs
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_signing::sign;
    use polar_signing::testing::{self as certs, Kind, Spec, now, root, signer};
    use serde_json::json;

    const AGENT: &str = "hello-agent";

    fn payload() -> Value {
        json!({
            "type": "configuration",
            "id": "hello-greeting",
            "target": {"agent": AGENT},
            "version": 7,
            "changes": [{"name": "GREETING", "value": "hello"}]
        })
    }

    struct Rig {
        ca: certs::Authority,
        verifier: UpdateVerifier,
    }

    fn rig() -> Rig {
        let ca = root("Polar Internal CA");
        let verifier = UpdateVerifier::new(
            Trust::from_pem(std::slice::from_ref(&ca.pem)).unwrap(),
            AGENT,
        )
        .with_clock(now);
        Rig { ca, verifier }
    }

    fn document(rig: &Rig, spec: Spec, payload: Value) -> Value {
        let s = signer(&rig.ca, spec);
        sign(payload, &[s.cert_pem], &s.key_pem).unwrap().to_value()
    }

    #[test]
    fn a_good_document_gives_the_update_and_who_signed_it() {
        let rig = rig();
        for kind in [Kind::P256, Kind::Ed25519] {
            let doc = document(
                &rig,
                Spec {
                    kind,
                    ..Spec::default()
                },
                payload(),
            );
            let verified = rig.verifier.verify(&doc).unwrap();
            assert_eq!(verified.id, "hello-greeting");
            assert_eq!(verified.version, 7);
            assert_eq!(
                verified.changes,
                vec![("GREETING".to_string(), "hello".to_string())]
            );
            assert_eq!(verified.signer.username.as_deref(), Some("alice"));
        }
    }

    #[test]
    fn an_unsigned_document_is_refused() {
        let rig = rig();
        // Just the payload, as an unsigned update would arrive.
        assert!(matches!(
            rig.verifier.verify(&payload()),
            Err(Rejection::Layout(LayoutError::Missing(_)))
        ));
        // A payload with a signature field that is empty.
        let doc = json!({"payload": payload(), "signature": {}});
        assert!(matches!(
            rig.verifier.verify(&doc),
            Err(Rejection::Layout(LayoutError::SignatureMissing(_)))
        ));
    }

    #[test]
    fn a_document_signed_by_someone_this_agent_does_not_trust_is_refused() {
        let rig = rig();
        let rogue = root("Rogue CA");
        let doc = document(
            &Rig {
                ca: rogue,
                verifier: UpdateVerifier::new(
                    Trust::from_pem(std::slice::from_ref(&rig.ca.pem)).unwrap(),
                    AGENT,
                ),
            },
            Spec::default(),
            payload(),
        );
        assert_eq!(
            rig.verifier.verify(&doc),
            Err(Rejection::Signature(VerifyError::UntrustedRoot))
        );
    }

    #[test]
    fn an_altered_payload_is_refused_whatever_was_changed() {
        let rig = rig();
        let doc = document(&rig, Spec::default(), payload());
        for edit in [
            |d: &mut Value| d["payload"]["version"] = json!(99),
            |d: &mut Value| d["payload"]["target"]["agent"] = json!("another-agent"),
            |d: &mut Value| d["payload"]["changes"][0]["value"] = json!("evil"),
        ] {
            let mut changed = doc.clone();
            edit(&mut changed);
            assert_eq!(
                rig.verifier.verify(&changed),
                Err(Rejection::Signature(VerifyError::BadSignature))
            );
        }
    }

    #[test]
    fn a_document_signed_for_another_agent_is_refused() {
        let rig = rig();
        let mut elsewhere = payload();
        elsewhere["target"]["agent"] = json!("another-agent");
        let doc = document(&rig, Spec::default(), elsewhere);
        assert_eq!(
            rig.verifier.verify(&doc),
            Err(Rejection::WrongTarget {
                wanted: AGENT.into(),
                got: "another-agent".into()
            })
        );
    }

    #[test]
    fn a_signer_who_may_only_change_policy_may_not_change_configuration() {
        let rig = rig();
        let doc = document(
            &rig,
            Spec {
                capabilities: vec!["c:update-policy"],
                ..Spec::default()
            },
            payload(),
        );
        assert!(matches!(
            rig.verifier.verify(&doc),
            Err(Rejection::Unauthorized(_))
        ));
        // The default signer holds update-configuration, and may.
        assert!(rig
            .verifier
            .verify(&document(&rig, Spec::default(), payload()))
            .is_ok());
    }

    #[test]
    fn a_signer_without_the_capability_is_refused() {
        let rig = rig();
        let doc = document(
            &rig,
            Spec {
                capabilities: vec!["c:grant"],
                ..Spec::default()
            },
            payload(),
        );
        assert!(matches!(
            rig.verifier.verify(&doc),
            Err(Rejection::Unauthorized(_))
        ));
    }

    #[test]
    fn a_signed_payload_that_is_not_an_update_is_refused() {
        let rig = rig();
        for bad in [
            json!({"type": "configuration", "id": "x"}),
            json!({"type": "configuration", "id": "x", "target": {"agent": AGENT}, "version": "7", "changes": []}),
            json!({"type": "configuration", "id": "x", "target": {"agent": AGENT}, "version": 1, "changes": [], "owner": "me"}),
            json!({"type": "configuration", "id": "x", "target": {"agent": AGENT, "region": "eu"}, "version": 1, "changes": []}),
            json!({"type": "configuration", "id": "x", "target": {"agent": AGENT}, "version": 1, "changes": [{"name": "A"}]}),
        ] {
            let doc = document(&rig, Spec::default(), bad.clone());
            assert!(
                matches!(rig.verifier.verify(&doc), Err(Rejection::Payload(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_document_of_another_kind_or_with_no_kind_is_not_a_configuration() {
        let rig = rig();
        // Otherwise a perfectly good update, signed by an authorised signer, but a command or an admission.
        for kind in ["command", "admission", "something-new"] {
            let mut payload = payload();
            payload["type"] = json!(kind);
            let doc = document(&rig, Spec::default(), payload);
            assert_eq!(
                rig.verifier.verify(&doc),
                Err(Rejection::WrongType {
                    wanted: "configuration",
                    got: kind.to_string()
                }),
                "{kind}"
            );
        }

        // No type at all, or one that is not text: not read as an update.
        for bad in [json!(null), json!(7), json!(["configuration"])] {
            let mut payload = payload();
            payload["type"] = bad;
            let doc = document(&rig, Spec::default(), payload);
            assert!(matches!(
                rig.verifier.verify(&doc),
                Err(Rejection::Payload(_))
            ));
        }
        let mut payload = payload();
        payload.as_object_mut().unwrap().remove("type");
        let doc = document(&rig, Spec::default(), payload);
        match rig.verifier.verify(&doc) {
            Err(Rejection::Payload(why)) => assert!(why.contains("type"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_kind_is_checked_before_the_target_so_a_command_for_another_agent_is_a_wrong_kind() {
        let rig = rig();
        let mut elsewhere = payload();
        elsewhere["type"] = json!("command");
        elsewhere["target"]["agent"] = json!("another-agent");
        let doc = document(&rig, Spec::default(), elsewhere);
        assert!(matches!(
            rig.verifier.verify(&doc),
            Err(Rejection::WrongType { .. })
        ));
    }

    #[test]
    fn an_expired_signing_certificate_is_refused() {
        let rig = rig();
        let doc = document(
            &rig,
            Spec {
                not_after_year: 2026,
                ..Spec::default()
            },
            payload(),
        );
        assert!(matches!(
            rig.verifier.verify(&doc),
            Err(Rejection::Signature(VerifyError::NotValidNow(_)))
        ));
    }

    #[test]
    fn a_revoked_signer_is_refused() {
        struct All;
        impl Revocation for All {
            fn is_revoked(&self, _: &SignerInfo) -> bool {
                true
            }
        }
        let rig = rig();
        let verifier = UpdateVerifier::new(
            Trust::from_pem(std::slice::from_ref(&rig.ca.pem)).unwrap(),
            AGENT,
        )
        .with_clock(now)
        .with_revocation(Arc::new(All));
        let doc = document(&rig, Spec::default(), payload());
        assert!(matches!(verifier.verify(&doc), Err(Rejection::Revoked(_))));
    }

    #[test]
    fn the_checks_run_in_the_order_the_design_gives() {
        // Wrong agent AND no capability: the target is checked first, so that is the reason given.
        let rig = rig();
        let mut elsewhere = payload();
        elsewhere["target"]["agent"] = json!("another-agent");
        let doc = document(
            &rig,
            Spec {
                capabilities: vec![],
                ..Spec::default()
            },
            elsewhere,
        );
        assert!(matches!(
            rig.verifier.verify(&doc),
            Err(Rejection::WrongTarget { .. })
        ));
    }

    #[test]
    fn a_pem_bundle_is_split_into_its_certificates() {
        let ca = root("A");
        let other = root("B");
        let bundle = format!("{}\n# a comment\n{}\n", ca.pem, other.pem);
        let parts = split_pem_bundle(&bundle);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(split_pem_bundle("nothing here").is_empty());
    }
}
