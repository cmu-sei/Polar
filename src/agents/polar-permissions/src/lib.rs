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
//!
//! A *principal* is more than its permissions: also the capabilities its certificate
//! carries (`c:` entries) and the groups it is in. Group membership is data about a
//! principal, so it lives here and not in the policy rules
//! (docs/architecture/authority-distribution.md): it is replaced whole by a signed
//! document, and policy reads it as the principal's parents. Nothing but this actor ever
//! supplies it.

use cert_issuer_common::capability::{Capability, extract_capabilities};
use cert_issuer_common::permission::{Permission, extract_permissions};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum PermissionsError {
    #[error("failed to extract permissions from the given certificate: {0}")]
    CertificateExtraction(String),
}

/// Everything the system holds about who a principal is, resolved by this actor and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub permissions: Vec<Permission>,
    /// The `c:` capabilities in the presented certificate. A policy may read them; the certificate is
    /// the floor they come from.
    pub capabilities: Vec<Capability>,
    /// The groups the identity is in: the latest signed membership's list for it when there is one, which may add to
    /// or take from the certificate's; otherwise the groups its certificate carries (`g:`).
    pub groups: Vec<String>,
    /// How groups and roles are arranged, as the latest signed groups document says, and which arrangement it is
    /// (counted up with each), so the policy actor builds Cedar's hierarchy from it once and not for each request.
    pub hierarchy: std::sync::Arc<cert_issuer_common::hierarchy::Hierarchy>,
    pub hierarchy_version: u64,
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
    /// The principal in full: its permissions as `ResolvePermissions` gives them, the capabilities its
    /// certificate carries, and the groups it is in.
    ResolvePrincipal {
        identity: String,
        certificate_pem: Option<String>,
        reply: RpcReplyPort<Result<Principal, PermissionsError>>,
    },
    /// Replaces group membership wholesale: for each identity, the groups it is in. The next version of the
    /// signed membership document, never a patch, so a removal is simply an absence.
    UpdateGroups {
        memberships: HashMap<String, Vec<String>>,
        /// How groups and roles are arranged: the rest of the same document.
        hierarchy: cert_issuer_common::hierarchy::Hierarchy,
        reply: RpcReplyPort<()>,
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
    groups: HashMap<String, Vec<String>>,
    hierarchy: std::sync::Arc<cert_issuer_common::hierarchy::Hierarchy>,
    hierarchy_version: u64,
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
            groups: HashMap::new(),
            hierarchy: Default::default(),
            hierarchy_version: 0,
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
            PermissionsMessage::ResolvePrincipal {
                identity,
                certificate_pem,
                reply,
            } => {
                let result = Self::resolve_principal(state, &identity, certificate_pem.as_deref());
                let _ = reply.send(result);
            }
            PermissionsMessage::UpdateGroups {
                memberships,
                hierarchy,
                reply,
            } => {
                state.groups = memberships;
                state.hierarchy = std::sync::Arc::new(hierarchy);
                state.hierarchy_version += 1;
                let _ = reply.send(());
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
    fn resolve_principal(
        state: &PermissionsActorState,
        identity: &str,
        certificate_pem: Option<&str>,
    ) -> Result<Principal, PermissionsError> {
        let capabilities = match certificate_pem {
            Some(pem) => extract_capabilities(pem)
                .map_err(|e| PermissionsError::CertificateExtraction(e.to_string()))?,
            None => Vec::new(),
        };
        // The certificate carries the catalog's membership (`g:`); the signed groups document, when it names the
        // identity, decides it instead, adding and removing alike (docs/architecture/policy-enforcement.md).
        let groups = match state.groups.get(identity) {
            Some(listed) => listed.clone(),
            None => match certificate_pem {
                Some(pem) => cert_issuer_common::group::extract_groups(pem)
                    .map_err(|e| PermissionsError::CertificateExtraction(e.to_string()))?
                    .into_iter()
                    .map(|g| g.0)
                    .collect(),
                None => Vec::new(),
            },
        };
        Ok(Principal {
            permissions: Self::resolve(state, identity, certificate_pem)?,
            capabilities,
            groups,
            hierarchy: state.hierarchy.clone(),
            hierarchy_version: state.hierarchy_version,
        })
    }

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

    // -- the principal: permissions, capabilities, and the groups it is in -----------------------

    async fn groups_of(
        actor: &ActorRef<PermissionsMessage>,
        who: &str,
        certificate_pem: Option<String>,
    ) -> Vec<String> {
        ractor::call!(actor, |reply| PermissionsMessage::ResolvePrincipal {
            identity: who.to_string(),
            certificate_pem,
            reply,
        })
        .unwrap()
        .unwrap()
        .groups
    }

    async fn listing(actor: &ActorRef<PermissionsMessage>, memberships: &[(&str, &[&str])]) {
        ractor::call!(actor, |reply| PermissionsMessage::UpdateGroups {
            memberships: memberships
                .iter()
                .map(|(who, groups)| (
                    who.to_string(),
                    groups.iter().map(|g| g.to_string()).collect()
                ))
                .collect(),
            hierarchy: Default::default(),
            reply,
        })
        .unwrap();
    }

    #[tokio::test]
    async fn a_certificate_s_groups_are_the_principal_s_until_the_groups_document_names_it() {
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();
        let workload = "kube-consumer.polar.serviceaccount.cluster.local";
        let certificate = build_test_certificate(vec![
            workload.to_string(),
            "g:cassini-client".to_string(),
            "g:encryption".to_string(),
        ]);
        // With no document, or one that does not name it, the certificate's groups stand.
        assert_eq!(
            groups_of(&actor, workload, Some(certificate.clone())).await,
            ["cassini-client", "encryption"]
        );
        listing(&actor, &[("someone-else", &["operators"])]).await;
        assert_eq!(
            groups_of(&actor, workload, Some(certificate.clone())).await,
            ["cassini-client", "encryption"]
        );
        // A document that names it decides its membership, adding and removing alike.
        listing(&actor, &[(workload, &["cassini-client", "auditors"])]).await;
        assert_eq!(
            groups_of(&actor, workload, Some(certificate.clone())).await,
            ["cassini-client", "auditors"]
        );
        listing(&actor, &[(workload, &[])]).await;
        assert!(
            groups_of(&actor, workload, Some(certificate))
                .await
                .is_empty()
        );
        // Without the certificate, only the document speaks.
        assert!(groups_of(&actor, workload, None).await.is_empty());
    }

    #[tokio::test]
    async fn a_principal_is_its_permissions_its_capabilities_and_its_groups() {
        use cert_issuer_common::capability::Capability;
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();
        ractor::call!(actor, |reply| PermissionsMessage::UpdateGroups {
            memberships: [(
                "alice".to_string(),
                vec!["credential-managers".to_string(), "operators".to_string()],
            )]
            .into(),
            hierarchy: Default::default(),
            reply,
        })
        .unwrap();
        let certificate_pem = build_test_certificate(vec![
            "alice.polar.svc.cluster.local".to_string(),
            Permission {
                scope: PermissionScope::UserToService,
                target: None,
            }
            .to_san_string(),
            Capability::UpdateRuntime.to_san_string(),
        ]);

        let principal = ractor::call!(actor, |reply| PermissionsMessage::ResolvePrincipal {
            identity: "alice".to_string(),
            certificate_pem: Some(certificate_pem),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(principal.permissions.len(), 1);
        assert_eq!(principal.capabilities, vec![Capability::UpdateRuntime]);
        assert_eq!(
            principal.groups,
            vec!["credential-managers".to_string(), "operators".to_string()]
        );
    }

    #[tokio::test]
    async fn groups_are_replaced_whole_and_belong_only_to_the_identity_they_name() {
        let (actor, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();
        let groups = |actor: &ActorRef<PermissionsMessage>, who: &str| {
            let (actor, who) = (actor.clone(), who.to_string());
            async move {
                ractor::call!(actor, |reply| PermissionsMessage::ResolvePrincipal {
                    identity: who.clone(),
                    certificate_pem: None,
                    reply,
                })
                .unwrap()
                .unwrap()
                .groups
            }
        };
        ractor::call!(actor, |reply| PermissionsMessage::UpdateGroups {
            memberships: [
                ("alice".to_string(), vec!["operators".to_string()]),
                ("bob".to_string(), vec!["operators".to_string()]),
            ]
            .into(),
            hierarchy: Default::default(),
            reply,
        })
        .unwrap();
        assert_eq!(groups(&actor, "alice").await, ["operators"]);
        assert!(
            groups(&actor, "carol").await.is_empty(),
            "no one else is in it"
        );

        // The next version replaces the membership whole: bob is no longer anywhere.
        ractor::call!(actor, |reply| PermissionsMessage::UpdateGroups {
            memberships: [("alice".to_string(), vec!["auditors".to_string()])].into(),
            hierarchy: Default::default(),
            reply,
        })
        .unwrap();
        assert_eq!(groups(&actor, "alice").await, ["auditors"]);
        assert!(groups(&actor, "bob").await.is_empty());
    }
}
