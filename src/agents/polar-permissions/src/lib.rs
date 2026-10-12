//! polar-permissions
//!
//! PermissionsActor -- the sole, real, authoritative source of "what
//! permissions does this identity actually hold." Deliberately,
//! completely separate from any real policy-decision logic (see
//! polar-policy's own PolicyActor, which consumes this actor's
//! resolutions but never resolves permissions itself) -- permissions
//! are real, structured *data* about what a principal has; policy is
//! a genuinely separate set of *rules* over that data. Keeping these
//! two concerns in two, separate crates is a deliberate choice, not
//! an accident of file layout.
//!
//! A resolved permission set is the union of two, real sources:
//! whatever is embedded in the identity's own, presented certificate
//! (via cert_issuer_common::permission::extract_permissions), and
//! whatever real, supplemental permissions have been pushed for that
//! identity via UpdateSupplementalPermissions -- the real,
//! gitops-driven path, mirroring PolicyActor's own UpdatePolicy
//! shape, letting an operator genuinely grant something immediately
//! without waiting for the next certificate reissuance.

use cert_issuer_common::permission::{Permission, extract_permissions};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum PermissionsError {
    #[error("failed to extract permissions from the given certificate: {0}")]
    CertificateExtraction(String),
}

pub enum PermissionsMessage {
    /// The real, sole way permissions are ever resolved for a given
    /// identity -- the union of `certificate_pem`'s own, real,
    /// embedded permissions (if given) and whatever real,
    /// supplemental permissions this actor already has cached for
    /// `identity`.
    ResolvePermissions {
        identity: String,
        certificate_pem: Option<String>,
        reply: RpcReplyPort<Result<Vec<Permission>, PermissionsError>>,
    },
    /// Replaces `identity`'s own, real, cached supplemental
    /// permissions wholesale -- the real, gitops-driven update path.
    /// Deliberately additive to whatever a real certificate already
    /// carries, never a replacement for it -- see this actor's own,
    /// top-level doc comment.
    UpdateSupplementalPermissions {
        identity: String,
        permissions: Vec<Permission>,
        reply: RpcReplyPort<()>,
    },
}

pub struct PermissionsActor;

pub struct PermissionsActorState {
    supplemental: HashMap<String, Vec<Permission>>,
}

#[async_trait]
impl Actor for PermissionsActor {
    type Msg = PermissionsMessage;
    type State = PermissionsActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(PermissionsActorState {
            supplemental: HashMap::new(),
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            PermissionsMessage::ResolvePermissions {
                identity,
                certificate_pem,
                reply,
            } => {
                let result = Self::resolve(state, &identity, certificate_pem.as_deref());
                let _ = reply.send(result);
            }
            PermissionsMessage::UpdateSupplementalPermissions {
                identity,
                permissions,
                reply,
            } => {
                state.supplemental.insert(identity, permissions);
                let _ = reply.send(());
            }
        }
        Ok(())
    }
}

impl PermissionsActor {
    /// The real, synchronous resolution logic itself -- extracted so
    /// it's directly, genuinely unit-testable without needing a real
    /// spawned actor or a real RpcReplyPort round trip at all.
    fn resolve(
        state: &PermissionsActorState,
        identity: &str,
        certificate_pem: Option<&str>,
    ) -> Result<Vec<Permission>, PermissionsError> {
        let mut resolved = match certificate_pem {
            Some(pem) => extract_permissions(pem)
                .map_err(|e| PermissionsError::CertificateExtraction(e.to_string()))?,
            None => Vec::new(),
        };

        if let Some(supplemental) = state.supplemental.get(identity) {
            resolved.extend(supplemental.iter().cloned());
        }

        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::permission::PermissionScope;

    fn build_test_certificate(sans: Vec<String>) -> String {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let private_key_pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                .unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params = rcgen::CertificateParams::new(sans).unwrap();
        params.self_signed(&key_pair).unwrap().pem()
    }

    #[tokio::test]
    async fn resolves_only_certificate_permissions_when_no_supplemental_exist() {
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();

        let cert_permission = Permission {
            scope: PermissionScope::ServiceToService,
            target: None,
        };
        let certificate_pem = build_test_certificate(vec![
            "test.polar.svc.cluster.local".to_string(),
            cert_permission.to_san_string(),
        ]);

        let resolved = ractor::call!(actor, |reply| PermissionsMessage::ResolvePermissions {
            identity: "test-agent".to_string(),
            certificate_pem: Some(certificate_pem),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(resolved, vec![cert_permission]);
    }

    #[tokio::test]
    async fn resolves_the_real_union_of_certificate_and_supplemental_permissions() {
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();

        let cert_permission = Permission {
            scope: PermissionScope::ServiceToService,
            target: None,
        };
        let supplemental_permission = Permission {
            scope: PermissionScope::UserToUser,
            target: Some("cassini.cluster.local".to_string()),
        };

        ractor::call!(actor, |reply| {
            PermissionsMessage::UpdateSupplementalPermissions {
                identity: "test-agent".to_string(),
                permissions: vec![supplemental_permission.clone()],
                reply,
            }
        })
        .unwrap();

        let certificate_pem = build_test_certificate(vec![
            "test.polar.svc.cluster.local".to_string(),
            cert_permission.to_san_string(),
        ]);

        let resolved = ractor::call!(actor, |reply| PermissionsMessage::ResolvePermissions {
            identity: "test-agent".to_string(),
            certificate_pem: Some(certificate_pem),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(resolved.len(), 2);
        assert!(resolved.contains(&cert_permission));
        assert!(resolved.contains(&supplemental_permission));
    }

    #[tokio::test]
    async fn resolves_only_supplemental_permissions_when_no_certificate_is_given() {
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();

        let supplemental_permission = Permission {
            scope: PermissionScope::UserToUser,
            target: None,
        };
        ractor::call!(actor, |reply| {
            PermissionsMessage::UpdateSupplementalPermissions {
                identity: "test-agent".to_string(),
                permissions: vec![supplemental_permission.clone()],
                reply,
            }
        })
        .unwrap();

        let resolved = ractor::call!(actor, |reply| PermissionsMessage::ResolvePermissions {
            identity: "test-agent".to_string(),
            certificate_pem: None,
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(resolved, vec![supplemental_permission]);
    }

    #[tokio::test]
    async fn supplemental_permissions_are_genuinely_scoped_to_their_own_identity() {
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();

        ractor::call!(actor, |reply| {
            PermissionsMessage::UpdateSupplementalPermissions {
                identity: "agent-a".to_string(),
                permissions: vec![Permission {
                    scope: PermissionScope::ServiceToService,
                    target: None,
                }],
                reply,
            }
        })
        .unwrap();

        let resolved_for_b = ractor::call!(actor, |reply| PermissionsMessage::ResolvePermissions {
            identity: "agent-b".to_string(),
            certificate_pem: None,
            reply,
        })
        .unwrap()
        .unwrap();

        assert!(
            resolved_for_b.is_empty(),
            "agent-a's own supplemental permissions must never leak to agent-b"
        );
    }

    #[tokio::test]
    async fn malformed_certificate_pem_is_a_real_error_not_a_panic() {
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();

        let result = ractor::call!(actor, |reply| PermissionsMessage::ResolvePermissions {
            identity: "test-agent".to_string(),
            certificate_pem: Some("not a real certificate".to_string()),
            reply,
        })
        .unwrap();

        assert!(matches!(
            result,
            Err(PermissionsError::CertificateExtraction(_))
        ));
    }
}
