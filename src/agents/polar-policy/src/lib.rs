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
//! decide *how* policy gets there, or *what* a given caller's own
//! real authorization question should be -- both are the caller's
//! own, real concern.

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
    /// The real, sole authorization query this actor answers.
    /// `principal`/`action`/`resource` are real Cedar entity UID
    /// strings (e.g. `Identity::"kube-consumer"`,
    /// `Action::"decrypt"`, `Secret::"some-secret-name"`) -- parsed
    /// internally. `context` is an arbitrary, real JSON document
    /// carrying whatever else a given policy might need to reference
    /// (e.g. the secret's own, real originating identity).
    CheckAuthorization {
        principal: String,
        action: String,
        resource: String,
        context: serde_json::Value,
        reply: RpcReplyPort<Result<Decision, PolicyError>>,
    },
}

pub struct PolicyActor;

pub struct PolicyActorState {
    policy_set: Option<PolicySet>,
    authorizer: Authorizer,
}

#[async_trait]
impl Actor for PolicyActor {
    type Msg = PolicyMessage;
    type State = PolicyActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(PolicyActorState {
            policy_set: None,
            authorizer: Authorizer::new(),
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
                principal,
                action,
                resource,
                context,
                reply,
            } => {
                let result = Self::evaluate(state, &principal, &action, &resource, context);
                let _ = reply.send(result);
            }
        }
        Ok(())
    }
}

impl PolicyActor {
    /// The real, synchronous evaluation logic itself -- extracted so
    /// it's directly, genuinely unit-testable without needing a real
    /// spawned actor or a real RpcReplyPort round trip at all.
    fn evaluate(
        state: &PolicyActorState,
        principal: &str,
        action: &str,
        resource: &str,
        context: serde_json::Value,
    ) -> Result<Decision, PolicyError> {
        let policy_set = state
            .policy_set
            .as_ref()
            .ok_or(PolicyError::NoPolicyLoaded)?;

        let principal_uid: EntityUid = principal
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("principal: {e}")))?;
        let action_uid: EntityUid = action
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("action: {e}")))?;
        let resource_uid: EntityUid = resource
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("resource: {e}")))?;
        let context = Context::from_json_value(context, None)
            .map_err(|e| PolicyError::RequestConstruction(format!("context: {e}")))?;

        let request = Request::new(principal_uid, action_uid, resource_uid, context, None)
            .map_err(|e| PolicyError::RequestConstruction(e.to_string()))?;

        let entities = Entities::empty();
        let response = state.authorizer.is_authorized(&request, policy_set, &entities);

        Ok(response.decision().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ractor::Actor;

    const REAL_POLICY: &str = r#"
        permit(
            principal == Identity::"kube-consumer",
            action == Action::"decrypt",
            resource == Secret::"a-plaintext-env-var"
        );
    "#;

    async fn spawn_with_real_policy() -> ActorRef<PolicyMessage> {
        let (actor, _) = Actor::spawn(None, PolicyActor, ()).await.unwrap();

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
            principal: r#"Identity::"kube-consumer""#.to_string(),
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            context: serde_json::json!({}),
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
            principal: r#"Identity::"some-other-agent""#.to_string(),
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            context: serde_json::json!({}),
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
            principal: r#"Identity::"kube-consumer""#.to_string(),
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-completely-different-secret""#.to_string(),
            context: serde_json::json!({}),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(decision, Decision::Deny);
    }

    #[tokio::test]
    async fn querying_before_any_policy_is_loaded_returns_a_real_error() {
        let (actor, _) = Actor::spawn(None, PolicyActor, ()).await.unwrap();

        let result = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            principal: r#"Identity::"kube-consumer""#.to_string(),
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            context: serde_json::json!({}),
            reply,
        })
        .unwrap();

        assert!(matches!(result, Err(PolicyError::NoPolicyLoaded)));
    }

    #[tokio::test]
    async fn malformed_policy_source_is_genuinely_rejected() {
        let (actor, _) = Actor::spawn(None, PolicyActor, ()).await.unwrap();

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
            principal: r#"Identity::"kube-consumer""#.to_string(),
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            context: serde_json::json!({}),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(old_decision, Decision::Deny);

        // The new policy's own principal must genuinely be permitted.
        let new_decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            principal: r#"Identity::"a-new-agent""#.to_string(),
            action: r#"Action::"decrypt""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            context: serde_json::json!({}),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(new_decision, Decision::Allow);
    }
}
