//! polar-policy
//!
//! PolicyActor -- a real, generic, in-process authorization engine,
//! wrapping AWS's own, natively-Rust, formally-verified Cedar policy
//! language (https://www.cedarpolicy.com) rather than reimplementing
//! RBAC/ABAC authorization logic by hand. Any agent needing to make a
//! genuine "is this principal authorized to perform this action on
//! this resource" decision can spawn its own PolicyActor instance,
//! exactly the same, established pattern as CryptoActor.
//!
//! Deliberately, genuinely narrow: this crate answers authorization
//! queries against a real Cedar PolicySet, and lets that PolicySet be
//! replaced wholesale (mirroring how policy will genuinely be
//! distributed -- a complete, real Cedar policy document pushed over
//! Cassini whenever it changes, not incremental edits). It does not
//! decide *how* policy gets there -- both are the caller's own, real
//! concern.
//!
//! Deliberately, permanently coupled to a real
//! polar_permissions::PermissionsActor -- not optional, by design.
//! Permissions are real, structured *data* about what a principal
//! has; policy is a genuinely separate set of *rules* over that
//! data, and this actor must never itself contain permissions-
//! resolution logic (see polar-permissions's own, top-level doc
//! comment). More importantly, this coupling closes a real,
//! structural tampering surface: an earlier design let a caller
//! supply arbitrary entity JSON directly, meaning a caller could, in
//! principle, fabricate a principal's own attributes and have them
//! silently accepted. This actor now derives a principal's own
//! attributes exclusively from PermissionsActor's own, real
//! resolution -- there is no path, for any caller, to supply or
//! influence a principal's own entity data at all.

use cedar_policy::{Authorizer, Context, Entities, EntityUid, PolicySet, Request};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, async_trait};

/// The real, complete, permanently-narrow decision this crate ever
/// returns -- deliberately not cedar_policy::Decision directly, so
/// no caller of this crate ever needs its own, direct dependency on
/// cedar-policy just to match on a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

impl From<cedar_policy::Decision> for Decision {
    fn from(d: cedar_policy::Decision) -> Self {
        match d {
            cedar_policy::Decision::Allow => Decision::Allow,
            cedar_policy::Decision::Deny => Decision::Deny,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("failed to parse policy source: {0}")]
    PolicyParse(String),
    #[error("failed to construct a real authorization request: {0}")]
    RequestConstruction(String),
    #[error("no policy set has been loaded yet")]
    NoPolicyLoaded,
    #[error("failed to resolve the requester's own real permissions: {0}")]
    PermissionsResolution(String),
}

pub enum PolicyMessage {
    /// Replaces this actor's own, current policy set wholesale --
    /// the real, sole way policy is ever updated. `policy_source` is
    /// real, raw Cedar policy syntax, not JSON -- exactly what the
    /// scheduler will genuinely read from a real, gitops-managed
    /// policy file and push over Cassini whenever it changes.
    UpdatePolicy {
        policy_source: String,
        reply: RpcReplyPort<Result<(), PolicyError>>,
    },
    /// The real, sole authorization query this actor answers. Every
    /// caller supplies only an identity and, optionally, that
    /// identity's own, real, presented certificate -- never any
    /// entity/attribute data directly; see this crate's own,
    /// top-level doc comment for why. `action`/`resource` are real
    /// Cedar entity UID strings (e.g. `Action::"decrypt"`,
    /// `Secret::"some-secret-name"`) -- parsed internally.
    CheckAuthorization {
        requester_identity: String,
        requester_certificate_pem: Option<String>,
        action: String,
        resource: String,
        reply: RpcReplyPort<Result<Decision, PolicyError>>,
    },
}

pub struct PolicyActorArgs {
    pub permissions_actor: ActorRef<polar_permissions::PermissionsMessage>,
}

pub struct PolicyActor;

pub struct PolicyActorState {
    policy_set: Option<PolicySet>,
    authorizer: Authorizer,
    permissions_actor: ActorRef<polar_permissions::PermissionsMessage>,
}

#[async_trait]
impl Actor for PolicyActor {
    type Msg = PolicyMessage;
    type State = PolicyActorState;
    type Arguments = PolicyActorArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: PolicyActorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(PolicyActorState {
            policy_set: None,
            authorizer: Authorizer::new(),
            permissions_actor: args.permissions_actor,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            PolicyMessage::UpdatePolicy {
                policy_source,
                reply,
            } => {
                let result = policy_source
                    .parse::<PolicySet>()
                    .map_err(|e| PolicyError::PolicyParse(e.to_string()))
                    .map(|parsed| {
                        state.policy_set = Some(parsed);
                    });
                let _ = reply.send(result);
            }
            PolicyMessage::CheckAuthorization {
                requester_identity,
                requester_certificate_pem,
                action,
                resource,
                reply,
            } => {
                let result = Self::evaluate(
                    state,
                    &requester_identity,
                    requester_certificate_pem,
                    &action,
                    &resource,
                )
                .await;
                let _ = reply.send(result);
            }
        }
        Ok(())
    }
}

impl PolicyActor {
    /// Converts a real, resolved `Permission` into its own, real
    /// Cedar-compatible JSON value -- deliberately not via
    /// `Permission`'s own, default Serde-derived shape, since that
    /// would emit a bare JSON `null` for an absent `target`, which
    /// Cedar's own entity-JSON parser genuinely, always rejects
    /// (confirmed directly against cedar-policy-core's own, real
    /// source: "JSON null, which is never valid"). `target` is
    /// therefore omitted entirely when absent -- a real Cedar policy
    /// can check for it with `has`, rather than ever receiving null.
    fn permission_to_json(
        permission: &cert_issuer_common::permission::Permission,
    ) -> serde_json::Value {
        let mut attrs = serde_json::Map::new();
        attrs.insert(
            "scope".to_string(),
            serde_json::Value::String(format!("{:?}", permission.scope)),
        );
        if let Some(target) = &permission.target {
            attrs.insert(
                "target".to_string(),
                serde_json::Value::String(target.clone()),
            );
        }
        serde_json::Value::Object(attrs)
    }

    /// The real, complete evaluation flow: resolve `requester_identity`'s
    /// own, real, authoritative permission set via this actor's own,
    /// required PermissionsActor, build that principal's own, real
    /// entity JSON directly from the resolution (never from anything
    /// a caller supplies), and evaluate. There is no path here for a
    /// caller to influence the principal's own attributes at all.
    async fn evaluate(
        state: &PolicyActorState,
        requester_identity: &str,
        requester_certificate_pem: Option<String>,
        action: &str,
        resource: &str,
    ) -> Result<Decision, PolicyError> {
        let policy_set = state
            .policy_set
            .as_ref()
            .ok_or(PolicyError::NoPolicyLoaded)?;

        let permissions = ractor::call!(state.permissions_actor, |reply| {
            polar_permissions::PermissionsMessage::ResolvePermissions {
                identity: requester_identity.to_string(),
                certificate_pem: requester_certificate_pem,
                reply,
            }
        })
        .map_err(|e| PolicyError::PermissionsResolution(e.to_string()))?
        .map_err(|e| PolicyError::PermissionsResolution(e.to_string()))?;

        let principal_uid_str = format!(r#"Identity::"{requester_identity}""#);
        let principal_uid: EntityUid = principal_uid_str
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("principal: {e}")))?;
        let action_uid: EntityUid = action
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("action: {e}")))?;
        let resource_uid: EntityUid = resource
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("resource: {e}")))?;

        let entities_json = serde_json::json!([
            {
                "uid": { "type": "Identity", "id": requester_identity },
                "attrs": {
                    "permissions": permissions.iter().map(Self::permission_to_json).collect::<Vec<_>>()
                },
                "parents": []
            }
        ]);
        let entities = Entities::from_json_value(entities_json, None)
            .map_err(|e| PolicyError::RequestConstruction(format!("entities: {e}")))?;

        let context = Context::empty();
        let request = Request::new(principal_uid, action_uid, resource_uid, context, None)
            .map_err(|e| PolicyError::RequestConstruction(e.to_string()))?;

        let response = state
            .authorizer
            .is_authorized(&request, policy_set, &entities);

        Ok(response.decision().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::permission::{Permission, PermissionScope};
    use ractor::Actor;

    const REAL_POLICY: &str = r#"
        permit(
            principal == Identity::"kube-consumer",
            action == Action::"decrypt",
            resource == Secret::"a-plaintext-env-var"
        );
    "#;

    async fn spawn_permissions_actor() -> ActorRef<polar_permissions::PermissionsMessage> {
        let (actor, _) = Actor::spawn(None, polar_permissions::PermissionsActor, ())
            .await
            .unwrap();
        actor
    }

    async fn spawn_policy_actor() -> ActorRef<PolicyMessage> {
        let permissions_actor = spawn_permissions_actor().await;
        let (actor, _) = Actor::spawn(None, PolicyActor, PolicyActorArgs { permissions_actor })
            .await
            .unwrap();
        actor
    }

    async fn spawn_with_real_policy() -> ActorRef<PolicyMessage> {
        let actor = spawn_policy_actor().await;

        let result = ractor::call!(actor, |reply| PolicyMessage::UpdatePolicy {
            policy_source: REAL_POLICY.to_string(),
            reply,
        })
        .unwrap();
        result.unwrap();

        actor
    }

    #[tokio::test]
    async fn a_genuinely_permitted_request_is_allowed() {
        let actor = spawn_with_real_policy().await;

        let decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(decision, Decision::Allow);
    }

    #[tokio::test]
    async fn a_request_from_a_different_principal_is_denied() {
        let actor = spawn_with_real_policy().await;

        let decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "some-other-agent".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(decision, Decision::Deny);
    }

    #[tokio::test]
    async fn a_request_for_a_different_resource_is_denied() {
        let actor = spawn_with_real_policy().await;

        let decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-completely-different-secret""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(decision, Decision::Deny);
    }

    #[tokio::test]
    async fn querying_before_any_policy_is_loaded_returns_a_real_error() {
        let actor = spawn_policy_actor().await;

        let result = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap();

        assert!(matches!(result, Err(PolicyError::NoPolicyLoaded)));
    }

    #[tokio::test]
    async fn malformed_policy_source_is_genuinely_rejected() {
        let actor = spawn_policy_actor().await;

        let result = ractor::call!(actor, |reply| PolicyMessage::UpdatePolicy {
            policy_source: "this is not real, valid Cedar syntax at all".to_string(),
            reply,
        })
        .unwrap();

        assert!(matches!(result, Err(PolicyError::PolicyParse(_))));
    }

    #[tokio::test]
    async fn updating_policy_genuinely_replaces_the_previous_one_wholesale() {
        let actor = spawn_with_real_policy().await;

        // A second, real update -- a genuinely different policy,
        // permitting a different principal entirely. Proves the
        // replacement is real and complete, not additive.
        let new_policy = r#"
            permit(
                principal == Identity::"a-new-agent",
                action == Action::"decrypt",
                resource == Secret::"a-plaintext-env-var"
            );
        "#;
        let result = ractor::call!(actor, |reply| PolicyMessage::UpdatePolicy {
            policy_source: new_policy.to_string(),
            reply,
        })
        .unwrap();
        result.unwrap();

        // The original policy's own principal must no longer be
        // permitted at all.
        let old_decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(old_decision, Decision::Deny);

        // The new policy's own principal must genuinely be permitted.
        let new_decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "a-new-agent".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(new_decision, Decision::Allow);
    }

    #[tokio::test]
    async fn a_policy_can_genuinely_reference_a_requesters_own_real_resolved_permissions() {
        // The actual, real, end-to-end proof of the whole, real
        // design this rewrite exists for: a real ABAC policy
        // referencing a requester's own permissions, resolved
        // exclusively through a real PermissionsActor -- never
        // supplied by the caller directly.
        let permissions_actor = spawn_permissions_actor().await;

        ractor::call!(permissions_actor, |reply| {
            polar_permissions::PermissionsMessage::UpdateSupplementalPermissions {
                identity: "kube-consumer".to_string(),
                permissions: vec![Permission {
                    scope: PermissionScope::ServiceToService,
                    target: None,
                }],
                reply,
            }
        })
        .unwrap();

        let (actor, _) = Actor::spawn(None, PolicyActor, PolicyActorArgs { permissions_actor })
            .await
            .unwrap();

        let policy = r#"
            permit(
                principal,
                action == Action::"decrypt",
                resource == Secret::"a-plaintext-env-var"
            ) when {
                principal.permissions.contains({"scope": "ServiceToService"})
            };
        "#;
        let result = ractor::call!(actor, |reply| PolicyMessage::UpdatePolicy {
            policy_source: policy.to_string(),
            reply,
        })
        .unwrap();
        result.unwrap();

        let allowed = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(allowed, Decision::Allow);

        // A genuinely different requester -- one PermissionsActor
        // has no real, resolved permissions for at all -- must be
        // denied, proving this isn't accidentally permitting anyone.
        let denied = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: "some-unrelated-agent".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(denied, Decision::Deny);
    }
}
