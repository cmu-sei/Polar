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

//! Who may sign a change: the policy first, the certificate second (docs/architecture/authority-distribution.md).
//!
//! The signature and the document's shape are checked elsewhere (`signed`). What is decided here is whether
//! this verified signer may make this change to this agent:
//!
//! 1. Once a policy is loaded, it is asked. `Allow` admits the signer whatever their certificate carries, and
//!    `Deny` refuses them whatever it carries.
//! 2. If it does not speak to the request (no rule applies), or no policy is loaded yet, the certificate's
//!    capability decides, as it always did.
//! 3. A rule that cannot be evaluated, or an answer that cannot be had, refuses: nothing here fails open.
//!
//! A change may touch several things (a configuration document names several variables); each is decided and
//! the signer must be admitted for all of them.

use polar_policy::{Decision, PolicyError, PolicyMessage};
use polar_signing::authority::AuthorityRefusal;
use ractor::ActorRef;

/// A Cedar entity reference, with the id escaped so that nothing in it can end the reference early or name
/// another entity. `None` for an id with a control character in it, which is not a name.
pub fn entity(kind: &str, id: &str) -> Option<String> {
    if id.chars().any(char::is_control) {
        return None;
    }
    Some(format!(
        "{kind}::\"{}\"",
        id.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// What is asked: this signer, this action, on each of these things.
#[derive(Debug, Clone)]
pub struct AuthorityRequest {
    /// The name policy knows the signer by: their username, or their identity if they have none.
    pub principal: String,
    /// The signer's leaf certificate, from which the principal's permissions and capabilities are read.
    pub certificate_pem: String,
    /// `update-configuration`, `update-runtime` or `run-command`.
    pub action: &'static str,
    /// The things the change touches, as `(entity type, id)`: a variable, a runtime section, a command.
    pub resources: Vec<(&'static str, String)>,
}

/// Decides one request. `floor` is the certificate's answer, asked only when policy does not decide.
pub async fn admit(
    policy: Option<&ActorRef<PolicyMessage>>,
    request: &AuthorityRequest,
    floor: impl Fn() -> Result<(), AuthorityRefusal>,
) -> Result<(), AuthorityRefusal> {
    for (kind, id) in &request.resources {
        let refuse = |why: &str| {
            AuthorityRefusal(format!(
                "{} may not {} {kind} `{id}`: {why}",
                request.principal, request.action
            ))
        };
        let (Some(action), Some(resource)) = (entity("Action", request.action), entity(kind, id))
        else {
            return Err(refuse("its name is not a name"));
        };

        let Some(policy) = policy else {
            floor()?;
            continue;
        };
        let answer = ractor::call!(policy, |reply| PolicyMessage::CheckAuthorization {
            facts: Default::default(),
            requester_identity: request.principal.clone(),
            requester_certificate_pem: Some(request.certificate_pem.clone()),
            action,
            resource,
            reply,
        });
        match answer {
            Ok(Ok(Decision::Allow)) => {}
            Ok(Ok(Decision::Deny)) => return Err(refuse("policy forbids it")),
            // Policy does not speak to it, or none is loaded yet: the certificate decides.
            Ok(Ok(Decision::NotDetermined)) | Ok(Err(PolicyError::NoPolicyLoaded)) => floor()?,
            Ok(Err(error)) => {
                tracing::warn!(%error, "policy could not be consulted, so the change is refused");
                return Err(refuse("policy could not be consulted"));
            }
            Err(error) => {
                tracing::warn!(%error, "the policy actor did not answer, so the change is refused");
                return Err(refuse("policy could not be consulted"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::capability::Capability;
    use polar_permissions::{PermissionsActor, PermissionsMessage};
    use polar_policy::{PolicyActor, PolicyActorArgs};
    use ractor::Actor;

    async fn policy(source: Option<&str>) -> ActorRef<PolicyMessage> {
        let (permissions, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();
        let (policy, _) = Actor::spawn(
            None,
            PolicyActor,
            PolicyActorArgs {
                permissions_actor: permissions,
            },
        )
        .await
        .unwrap();
        if let Some(source) = source {
            load(&policy, source).await;
        }
        policy
    }

    async fn load(policy: &ActorRef<PolicyMessage>, source: &str) {
        ractor::call!(policy, |reply| PolicyMessage::UpdatePolicy {
            policy_source: source.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
    }

    fn certificate(capabilities: &[Capability]) -> String {
        let mut sans = vec!["alice.polar.svc.cluster.local".to_string()];
        sans.extend(capabilities.iter().map(Capability::to_san_string));
        let mut rng = rand::rng();
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&key, rsa::pkcs8::LineEnding::LF).unwrap();
        rcgen::CertificateParams::new(sans)
            .unwrap()
            .self_signed(&rcgen::KeyPair::from_pem(&pem).unwrap())
            .unwrap()
            .pem()
    }

    fn request(
        principal: &str,
        capabilities: &[Capability],
        resources: &[&str],
    ) -> AuthorityRequest {
        AuthorityRequest {
            principal: principal.to_string(),
            certificate_pem: certificate(capabilities),
            action: "update-runtime",
            resources: resources
                .iter()
                .map(|r| ("Section", r.to_string()))
                .collect(),
        }
    }

    fn certificate_allows() -> Result<(), AuthorityRefusal> {
        Ok(())
    }

    fn certificate_refuses() -> Result<(), AuthorityRefusal> {
        Err(AuthorityRefusal("the certificate does not carry it".into()))
    }

    const PERMIT_ALICE: &str = r#"permit(principal == Identity::"alice", action == Action::"update-runtime", resource == Section::"a/b");"#;
    const FORBID_ALICE: &str = r#"forbid(principal == Identity::"alice", action == Action::"update-runtime", resource == Section::"a/b");"#;

    #[tokio::test]
    async fn with_no_policy_the_certificate_decides_as_it_always_did() {
        let req = request("alice", &[], &["a/b"]);
        // No policy actor at all, and a policy actor that has been given nothing.
        for actor in [None, Some(policy(None).await)] {
            assert_eq!(
                admit(actor.as_ref(), &req, certificate_allows).await,
                Ok(())
            );
            assert!(
                admit(actor.as_ref(), &req, certificate_refuses)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn a_permit_admits_a_signer_whose_certificate_would_not() {
        let actor = policy(Some(PERMIT_ALICE)).await;
        let req = request("alice", &[], &["a/b"]);
        assert_eq!(admit(Some(&actor), &req, certificate_refuses).await, Ok(()));
    }

    #[tokio::test]
    async fn a_forbid_refuses_a_signer_whose_certificate_would_admit() {
        let actor = policy(Some(FORBID_ALICE)).await;
        let req = request("alice", &[Capability::UpdateRuntime], &["a/b"]);
        let refusal = admit(Some(&actor), &req, certificate_allows)
            .await
            .unwrap_err();
        assert!(
            refusal.0.contains("alice") && refusal.0.contains("a/b"),
            "{refusal}"
        );
        assert!(refusal.0.contains("policy"), "{refusal}");
    }

    #[tokio::test]
    async fn a_policy_that_does_not_speak_to_the_request_leaves_it_to_the_certificate() {
        // The rules mention alice and a/b only; this is bob, on something else.
        let actor = policy(Some(PERMIT_ALICE)).await;
        let req = request("bob", &[], &["x/y"]);
        assert_eq!(admit(Some(&actor), &req, certificate_allows).await, Ok(()));
        assert!(
            admit(Some(&actor), &req, certificate_refuses)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn every_thing_a_change_touches_must_be_admitted() {
        let actor = policy(Some(&format!(
            "{PERMIT_ALICE}\n{}",
            r#"forbid(principal, action, resource == Section::"a/locked");"#
        )))
        .await;
        // Admitted by policy for one, by the certificate for the other.
        let both = request("alice", &[], &["a/b", "a/other"]);
        assert_eq!(admit(Some(&actor), &both, certificate_allows).await, Ok(()));
        assert!(
            admit(Some(&actor), &both, certificate_refuses)
                .await
                .is_err()
        );
        // One forbidden refuses the whole change, whatever the certificate says of the rest.
        let one_locked = request("alice", &[], &["a/b", "a/locked"]);
        let refusal = admit(Some(&actor), &one_locked, certificate_allows)
            .await
            .unwrap_err();
        assert!(refusal.0.contains("a/locked"), "{refusal}");
    }

    #[tokio::test]
    async fn a_rule_that_cannot_be_evaluated_refuses_whatever_the_certificate_says() {
        let actor = policy(Some(
            r#"forbid(principal, action, resource) when { context.now_secs * 9223372036854775807 > 0 };"#,
        ))
        .await;
        let req = request("alice", &[Capability::UpdateRuntime], &["a/b"]);
        assert!(admit(Some(&actor), &req, certificate_allows).await.is_err());
    }

    #[tokio::test]
    async fn a_rule_can_read_the_groups_and_the_capabilities_of_the_signer() {
        let (permissions, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();
        ractor::call!(permissions, |reply| PermissionsMessage::UpdateGroups {
            memberships: [("alice".to_string(), vec!["git-authors".to_string()])].into(),
            reply,
        })
        .unwrap();
        let (actor, _) = Actor::spawn(
            None,
            PolicyActor,
            PolicyActorArgs {
                permissions_actor: permissions,
            },
        )
        .await
        .unwrap();
        load(
            &actor,
            r#"permit(principal in Group::"git-authors", action == Action::"update-runtime", resource)
                 when { principal in Capability::"update-runtime" };"#,
        )
        .await;
        // In the group and holding the capability: admitted by policy (the certificate floor refuses).
        let both = request("alice", &[Capability::UpdateRuntime], &["a/b"]);
        assert_eq!(
            admit(Some(&actor), &both, certificate_refuses).await,
            Ok(())
        );
        // In the group without the capability, and a stranger with it: policy is silent, the floor decides.
        let no_capability = request("alice", &[], &["a/b"]);
        assert!(
            admit(Some(&actor), &no_capability, certificate_refuses)
                .await
                .is_err()
        );
        let stranger = request("mallory", &[Capability::UpdateRuntime], &["a/b"]);
        assert!(
            admit(Some(&actor), &stranger, certificate_refuses)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_id_is_a_name_and_never_syntax() {
        // The rule permits exactly Section::"x/y". An id built to look like the end of that reference and more
        // must be a different, unmatched thing, not a way to be admitted.
        let actor = policy(Some(
            r#"permit(principal, action == Action::"update-runtime", resource == Section::"x/y");"#,
        ))
        .await;
        for id in ["x/y\" || true || \"", "x/y\"", "x/y\\", "x/y\nz"] {
            let req = AuthorityRequest {
                principal: "alice".into(),
                certificate_pem: certificate(&[]),
                action: "update-runtime",
                resources: vec![("Section", id.to_string())],
            };
            assert!(
                admit(Some(&actor), &req, certificate_refuses)
                    .await
                    .is_err(),
                "{id:?}"
            );
        }
        assert_eq!(
            entity("Section", "plain/id").unwrap(),
            r#"Section::"plain/id""#
        );
        assert_eq!(
            entity("Section", "q\"uote").unwrap(),
            r#"Section::"q\"uote""#
        );
        assert!(entity("Section", "line\nbreak").is_none());
    }
}
