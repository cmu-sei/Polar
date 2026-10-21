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

pub mod schema;

/// What the rules say about a request, which is one of three things, not two.
///
/// Cedar denies by default, so "no rule mentions this" and "a rule forbids this" both come back as a denial
/// from Cedar. They are different answers. The first means the policy does not speak to the request, and
/// whoever asked falls back to what they had before policy (for a signer, the capabilities in the certificate);
/// the second is the policy overriding it. A caller that has no fallback (a secret has none) treats anything
/// but `Allow` as a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// A rule permits it, and none forbids it.
    Allow,
    /// A rule forbids it, or the rules could not be evaluated: refused, with no fallback.
    Deny,
    /// No rule applies. Neither permitted nor forbidden.
    NotDetermined,
}

/// The most policy text the agents will take: more than any rule set should need, and a bound on what a
/// signed document can ask every agent to parse.
pub const MAX_POLICY_BYTES: usize = 256 * 1024;

/// Whether `source` is Cedar policy this crate would load, with the reason if not: it parses, and it speaks the
/// schema ([`schema`]), strictly. Pure: an agent checks a policy document with it when it arrives, and `polar-sign`
/// before it is signed and sent to every other agent.
pub fn validate_source(source: &str) -> Result<(), String> {
    load_source(source).map(|_| ())
}

/// The policy in `source`, parsed and checked against the schema.
fn load_source(source: &str) -> Result<PolicySet, String> {
    if source.len() > MAX_POLICY_BYTES {
        return Err(format!(
            "the policy is too large: {} bytes, and at most {MAX_POLICY_BYTES} are taken",
            source.len()
        ));
    }
    let policies = source.parse::<PolicySet>().map_err(|e| e.to_string())?;
    schema::validate(&policies).map_err(|problems| {
        format!(
            "the policy does not speak Polar's schema: {}",
            problems.join("; ")
        )
    })?;
    Ok(policies)
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
        /// What the asker knows about the resource that no rule document may say.
        facts: Facts,
        reply: RpcReplyPort<Result<Decision, PolicyError>>,
    },
}

/// What the asker knows about the resource from its own records, handed to the rules as the resource's
/// attributes. They are reserved: a policy document cannot set them, because the evaluator is given them
/// by the asker and by nothing else. The principal has no counterpart; its facts come from PermissionsActor
/// only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// The identity that owns the resource (for a secret, the one that escrowed it), as `Identity::"owner"`
    /// in the attribute `owner`, so `resource.owner == principal` says "the requester owns it".
    pub owner: Option<String>,
    /// The identity whose namespace the resource's name is in, as `Identity::"namespace"` in the attribute
    /// `namespace`, so `resource.namespace == principal` says "the requester is storing under its own name". The
    /// asker computes it from the name; it is absent when the name has no namespace, and a rule on it then cannot
    /// be satisfied.
    pub namespace: Option<String>,
    /// The resource's labels, as its tags (`resource.hasTag("kind") && resource.getTag("kind") == "token"`). Tags
    /// are apart from attributes, so a label can never take the place of a fact above.
    pub labels: std::collections::BTreeMap<String, String>,
    /// For a topic (`publish`, `subscribe`): whether it is in the requester's declared or implied set.
    pub topic: Option<TopicFacts>,
}

/// Whether a topic is in the requester's own set, and how (docs/architecture/topics.md): `context.declared` (family
/// topics included) and `context.implied`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TopicFacts {
    pub declared: bool,
    pub implied: bool,
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
                // A policy that does not parse or does not speak the schema is refused whole, and the one in
                // force stays.
                let result = load_source(&policy_source)
                    .map_err(PolicyError::PolicyParse)
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
                facts,
                reply,
            } => {
                let result = Self::evaluate(
                    state,
                    &requester_identity,
                    requester_certificate_pem,
                    &action,
                    &resource,
                    &facts,
                )
                .await;
                let _ = reply.send(result);
            }
        }
        Ok(())
    }
}

impl PolicyActor {
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
        facts: &Facts,
    ) -> Result<Decision, PolicyError> {
        let policy_set = state
            .policy_set
            .as_ref()
            .ok_or(PolicyError::NoPolicyLoaded)?;

        let principal = ractor::call!(state.permissions_actor, |reply| {
            polar_permissions::PermissionsMessage::ResolvePrincipal {
                identity: requester_identity.to_string(),
                certificate_pem: requester_certificate_pem,
                reply,
            }
        })
        .map_err(|e| PolicyError::PermissionsResolution(e.to_string()))?
        .map_err(|e| PolicyError::PermissionsResolution(e.to_string()))?;

        // Built from the type and the id as they are, never by formatting the id into text to be parsed: a
        // name with a quote or a backslash in it is that name, and no other entity.
        let principal_uid = EntityUid::from_type_name_and_id(
            "Identity"
                .parse::<cedar_policy::EntityTypeName>()
                .map_err(|e| PolicyError::RequestConstruction(format!("principal: {e}")))?,
            cedar_policy::EntityId::new(requester_identity),
        );
        let action_uid: EntityUid = action
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("action: {e}")))?;
        let resource_uid: EntityUid = resource
            .parse()
            .map_err(|e| PolicyError::RequestConstruction(format!("resource: {e}")))?;

        // The principal is built from what PermissionsActor resolved and from nothing a caller said. Its
        // groups and its capabilities are its parents, so a rule can say `principal in Group::"operators"` or
        // `principal in Capability::"update-policy"`; its workload, when its identity is a workload's, is
        // `principal.workload`.
        let mut principal_attrs = serde_json::Map::new();
        if let Some(workload) = cert_issuer_common::identity::workload_of(requester_identity) {
            principal_attrs.insert(
                "workload".to_string(),
                serde_json::Value::String(workload.to_string()),
            );
        }
        let parents: Vec<serde_json::Value> = principal
            .groups
            .iter()
            .map(|g| serde_json::json!({ "type": "Group", "id": g }))
            .chain(principal.capabilities.iter().map(|c| {
                serde_json::json!({
                    "type": "Capability",
                    "id": c.to_san_string().trim_start_matches("c:"),
                })
            }))
            .chain(principal.permissions.iter().map(|p| {
                serde_json::json!({
                    "type": "Permission",
                    "id": p.to_san_string().trim_start_matches("p:"),
                })
            }))
            .collect();
        let mut entities_json = vec![serde_json::json!({
            "uid": { "type": "Identity", "id": requester_identity },
            "attrs": principal_attrs,
            "parents": parents,
        })];
        // The resource, with the attributes its records give it. Built from the parsed id and not from the
        // text, like the principal. Only the attributes that exist are given, so a rule on one that does not
        // cannot be satisfied.
        if facts.owner.is_some() || facts.namespace.is_some() || !facts.labels.is_empty() {
            let mut attrs = serde_json::Map::new();
            let tags: serde_json::Map<String, serde_json::Value> = facts
                .labels
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                .collect();
            if let Some(owner) = &facts.owner {
                attrs.insert(
                    "owner".to_string(),
                    serde_json::json!({ "__entity": { "type": "Identity", "id": owner } }),
                );
            }
            if let Some(namespace) = &facts.namespace {
                attrs.insert(
                    "namespace".to_string(),
                    serde_json::json!({ "__entity": { "type": "Identity", "id": namespace } }),
                );
            }
            entities_json.push(serde_json::json!({
                "uid": {
                    "type": resource_uid.type_name().to_string(),
                    "id": resource_uid.id().unescaped(),
                },
                "attrs": attrs,
                "parents": [],
                "tags": tags,
            }));
        }
        let schema = schema::schema();
        let entities =
            Entities::from_json_value(serde_json::Value::Array(entities_json), Some(schema))
                .map_err(|e| PolicyError::RequestConstruction(format!("entities: {e}")))?;

        // The time is this actor's own, never a caller's.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut context = serde_json::json!({ "now_secs": now_secs });
        if let Some(topic) = facts.topic {
            context["declared"] = serde_json::Value::Bool(topic.declared);
            context["implied"] = serde_json::Value::Bool(topic.implied);
        }
        let context = Context::from_json_value(context, Some((schema, &action_uid)))
            .map_err(|e| PolicyError::RequestConstruction(format!("context: {e}")))?;
        let request = Request::new(
            principal_uid,
            action_uid,
            resource_uid,
            context,
            Some(schema),
        )
        .map_err(|e| PolicyError::RequestConstruction(e.to_string()))?;

        let response = state
            .authorizer
            .is_authorized(&request, policy_set, &entities);

        Ok(Self::decide(&response))
    }

    /// The three answers out of Cedar's two. A rule that could not be evaluated is a denial: it may have
    /// been the one that forbade the request, and Cedar would otherwise skip it. A denial with no rule
    /// behind it is not a denial but silence.
    fn decide(response: &cedar_policy::Response) -> Decision {
        if response.diagnostics().errors().next().is_some() {
            tracing::warn!(
                "a policy rule could not be evaluated, so the request is denied: {:?}",
                response.diagnostics().errors().collect::<Vec<_>>()
            );
            return Decision::Deny;
        }
        match response.decision() {
            cedar_policy::Decision::Allow => Decision::Allow,
            cedar_policy::Decision::Deny if response.diagnostics().reason().next().is_some() => {
                Decision::Deny
            }
            cedar_policy::Decision::Deny => Decision::NotDetermined,
        }
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
            action == Action::"retrieve",
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
            facts: Default::default(),
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
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
            facts: Default::default(),
            requester_identity: "some-other-agent".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();

        // No rule mentions this principal: the rules have no answer, which is not a denial.
        assert_eq!(decision, Decision::NotDetermined);
    }

    #[tokio::test]
    async fn a_request_for_a_different_resource_is_denied() {
        let actor = spawn_with_real_policy().await;

        let decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            facts: Default::default(),
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
            resource: r#"Secret::"a-completely-different-secret""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();

        assert_eq!(decision, Decision::NotDetermined);
    }

    #[tokio::test]
    async fn querying_before_any_policy_is_loaded_returns_a_real_error() {
        let actor = spawn_policy_actor().await;

        let result = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            facts: Default::default(),
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
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
                action == Action::"retrieve",
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
            facts: Default::default(),
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(old_decision, Decision::NotDetermined);

        // The new policy's own principal must genuinely be permitted.
        let new_decision = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            facts: Default::default(),
            requester_identity: "a-new-agent".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
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
                action == Action::"retrieve",
                resource == Secret::"a-plaintext-env-var"
            ) when {
                principal in Permission::"s2s"
            };
        "#;
        let result = ractor::call!(actor, |reply| PolicyMessage::UpdatePolicy {
            policy_source: policy.to_string(),
            reply,
        })
        .unwrap();
        result.unwrap();

        let allowed = ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            facts: Default::default(),
            requester_identity: "kube-consumer".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
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
            facts: Default::default(),
            requester_identity: "some-unrelated-agent".to_string(),
            requester_certificate_pem: None,
            action: r#"Action::"retrieve""#.to_string(),
            resource: r#"Secret::"a-plaintext-env-var""#.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
        assert_eq!(denied, Decision::NotDetermined);
    }

    // -- what a rule can say: forbid, groups, capabilities, and an answer that is not a denial -----

    async fn load(actor: &ActorRef<PolicyMessage>, source: &str) {
        ractor::call!(actor, |reply| PolicyMessage::UpdatePolicy {
            policy_source: source.to_string(),
            reply,
        })
        .unwrap()
        .unwrap();
    }

    async fn ask(
        actor: &ActorRef<PolicyMessage>,
        identity: &str,
        certificate_pem: Option<String>,
        action: &str,
        resource: &str,
    ) -> Decision {
        ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            facts: Default::default(),
            requester_identity: identity.to_string(),
            requester_certificate_pem: certificate_pem,
            action: action.to_string(),
            resource: resource.to_string(),
            reply,
        })
        .unwrap()
        .unwrap()
    }

    #[tokio::test]
    async fn a_forbid_is_a_denial_that_beats_a_permit() {
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"
            permit(principal, action == Action::"retrieve", resource);
            forbid(principal, action == Action::"retrieve", resource == Secret::"frozen-one");
            "#,
        )
        .await;
        let allowed = ask(
            &actor,
            "a",
            None,
            r#"Action::"retrieve""#,
            r#"Secret::"ordinary""#,
        )
        .await;
        let forbidden = ask(
            &actor,
            "a",
            None,
            r#"Action::"retrieve""#,
            r#"Secret::"frozen-one""#,
        )
        .await;
        assert_eq!(allowed, Decision::Allow);
        assert_eq!(
            forbidden,
            Decision::Deny,
            "an explicit forbid is an answer, and it is no"
        );
    }

    #[tokio::test]
    async fn a_rule_that_cannot_be_evaluated_denies_and_never_lets_a_request_through() {
        let actor = spawn_policy_actor().await;
        // Speaks the schema, so it loads, but overflows when it is evaluated: an error.
        load(
            &actor,
            r#"
            forbid(principal, action, resource) when { context.now_secs * 9223372036854775807 > 0 };
            permit(principal, action == Action::"retrieve", resource);
            "#,
        )
        .await;
        let decision = ask(&actor, "a", None, r#"Action::"retrieve""#, r#"Secret::"x""#).await;
        assert_eq!(
            decision,
            Decision::Deny,
            "a rule that errors might have been the one that forbade it"
        );
    }

    #[tokio::test]
    async fn group_membership_is_data_about_the_principal_that_only_permissions_supplies() {
        let permissions_actor = spawn_permissions_actor().await;
        ractor::call!(permissions_actor, |reply| {
            polar_permissions::PermissionsMessage::UpdateGroups {
                memberships: [("alice".to_string(), vec!["credential-managers".to_string()])]
                    .into(),
                reply,
            }
        })
        .unwrap();
        let (actor, _) = Actor::spawn(None, PolicyActor, PolicyActorArgs { permissions_actor })
            .await
            .unwrap();
        load(
            &actor,
            r#"permit(principal in Group::"credential-managers", action == Action::"retrieve", resource);"#,
        )
        .await;

        let member = ask(
            &actor,
            "alice",
            None,
            r#"Action::"retrieve""#,
            r#"Secret::"s""#,
        )
        .await;
        let stranger = ask(
            &actor,
            "bob",
            None,
            r#"Action::"retrieve""#,
            r#"Secret::"s""#,
        )
        .await;
        assert_eq!(member, Decision::Allow);
        assert_eq!(stranger, Decision::NotDetermined);
    }

    #[tokio::test]
    async fn a_rule_can_read_the_capabilities_the_certificate_carries() {
        use cert_issuer_common::capability::Capability;
        let certificate = |capabilities: &[Capability]| {
            let mut sans = vec!["x.polar.svc.cluster.local".to_string()];
            sans.extend(capabilities.iter().map(Capability::to_san_string));
            let mut rng = rand::rng();
            let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
            let pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&key, rsa::pkcs8::LineEnding::LF)
                .unwrap();
            rcgen::CertificateParams::new(sans)
                .unwrap()
                .self_signed(&rcgen::KeyPair::from_pem(&pem).unwrap())
                .unwrap()
                .pem()
        };
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"permit(principal, action == Action::"update-runtime", resource)
               when { principal in Capability::"update-runtime" };"#,
        )
        .await;
        let holder = ask(
            &actor,
            "alice",
            Some(certificate(&[Capability::UpdateRuntime])),
            r#"Action::"update-runtime""#,
            r#"Section::"git-repo-controller/repositories""#,
        )
        .await;
        let without = ask(
            &actor,
            "alice",
            Some(certificate(&[Capability::Grant])),
            r#"Action::"update-runtime""#,
            r#"Section::"git-repo-controller/repositories""#,
        )
        .await;
        assert_eq!(holder, Decision::Allow);
        assert_eq!(without, Decision::NotDetermined);
    }

    #[tokio::test]
    async fn a_name_is_exactly_the_entity_it_says_and_two_names_never_collapse_into_one() {
        let actor = spawn_policy_actor().await;
        // Permits the principal whose name is a"b, quote included.
        load(
            &actor,
            r#"permit(principal == Identity::"a\"b", action == Action::"subscribe", resource);"#,
        )
        .await;
        let quote = ask_topic(&actor, "a\"b").await;
        // A different name, backslash then quote: formatted without escaping it parsed as the same entity.
        let backslash_quote = ask_topic(&actor, "a\\\"b").await;
        assert_eq!(quote, Decision::Allow);
        assert_eq!(backslash_quote, Decision::NotDetermined);
    }

    async fn ask_topic(actor: &ActorRef<PolicyMessage>, who: &str) -> Decision {
        ask_with(
            actor,
            who,
            r#"Action::"subscribe""#,
            r#"Topic::"t""#,
            Facts {
                topic: Some(TopicFacts::default()),
                ..Default::default()
            },
        )
        .await
    }

    #[tokio::test]
    async fn a_topic_request_says_whether_the_topic_is_the_requester_s_declared_or_implied() {
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"permit (principal, action == Action::"publish", resource) when { context.declared };
               forbid (principal, action == Action::"publish", resource) when { context.implied };"#,
        )
        .await;
        let publish = |declared, implied| {
            let actor = actor.clone();
            async move {
                ask_with(
                    &actor,
                    "kube-observer.polar.serviceaccount.cluster.local",
                    r#"Action::"publish""#,
                    r#"Topic::"kubernetes.cluster.consumer.Pod""#,
                    Facts {
                        topic: Some(TopicFacts { declared, implied }),
                        ..Default::default()
                    },
                )
                .await
            }
        };
        assert_eq!(publish(true, false).await, Decision::Allow);
        assert_eq!(publish(false, false).await, Decision::NotDetermined);
        assert_eq!(publish(false, true).await, Decision::Deny);
    }

    #[tokio::test]
    async fn a_workload_s_identity_names_its_workload_and_a_person_s_names_none() {
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"permit (principal, action == Action::"subscribe", resource)
               when { principal has workload && principal.workload == "kube-consumer" };"#,
        )
        .await;
        assert_eq!(
            ask_topic(&actor, "kube-consumer.polar.serviceaccount.cluster.local").await,
            Decision::Allow
        );
        assert_eq!(
            ask_topic(&actor, "kube-observer.polar.serviceaccount.cluster.local").await,
            Decision::NotDetermined
        );
        // A username is not a workload's identity, and has no workload.
        assert_eq!(ask_topic(&actor, "alice").await, Decision::NotDetermined);
    }

    #[tokio::test]
    async fn a_policy_that_does_not_speak_the_schema_is_refused_whole_and_the_one_in_force_stays() {
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"permit (principal, action == Action::"subscribe", resource);"#,
        )
        .await;
        let refused = ractor::call!(actor, |reply| PolicyMessage::UpdatePolicy {
            // The first rule is sound; the second reads a label as an attribute.
            policy_source: r#"forbid (principal, action == Action::"subscribe", resource);
                permit (principal, action == Action::"retrieve", resource) when { resource.kind == "token" };"#
                .to_string(),
            reply,
        })
        .unwrap();
        assert!(
            matches!(&refused, Err(PolicyError::PolicyParse(why)) if why.contains("schema")),
            "{refused:?}"
        );
        // The forbid did not come in with it: the old policy still decides.
        assert_eq!(ask_topic(&actor, "anyone").await, Decision::Allow);
    }

    #[test]
    fn policy_source_is_checked_without_an_actor_and_with_every_problem_said() {
        assert_eq!(
            validate_source(r#"permit(principal, action, resource);"#),
            Ok(())
        );
        // No rules is a policy that says nothing: valid, and every request is then undetermined.
        assert_eq!(validate_source(""), Ok(()));
        let problem = validate_source("this is not Cedar").unwrap_err();
        assert!(!problem.is_empty());
        // Larger than any rule set should be.
        let huge = "// ".to_string() + &"x".repeat(MAX_POLICY_BYTES);
        assert!(validate_source(&huge).unwrap_err().contains("too large"));
    }

    async fn ask_with(
        actor: &ActorRef<PolicyMessage>,
        identity: &str,
        action: &str,
        resource: &str,
        facts: Facts,
    ) -> Decision {
        ractor::call!(actor, |reply| PolicyMessage::CheckAuthorization {
            requester_identity: identity.to_string(),
            requester_certificate_pem: None,
            action: action.to_string(),
            resource: resource.to_string(),
            facts,
            reply,
        })
        .unwrap()
        .unwrap()
    }

    fn owned_by(owner: &str) -> Facts {
        Facts {
            owner: Some(owner.to_string()),
            ..Default::default()
        }
    }

    fn in_namespace(namespace: &str) -> Facts {
        Facts {
            namespace: Some(namespace.to_string()),
            ..Default::default()
        }
    }

    fn labelled(pairs: &[(&str, &str)]) -> Facts {
        Facts {
            labels: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    const STORE_IN_OWN_NAMESPACE: &str = r#"permit (principal, action == Action::"store", resource) when { resource has namespace && resource.namespace == principal };"#;

    #[tokio::test]
    async fn a_rule_on_the_namespace_lets_an_identity_store_in_its_own_and_not_another() {
        let actor = spawn_policy_actor().await;
        load(&actor, STORE_IN_OWN_NAMESPACE).await;
        let secret = r#"Secret::"git-repo-observer/github""#;
        assert_eq!(
            ask_with(
                &actor,
                "git-repo-observer",
                r#"Action::"store""#,
                secret,
                in_namespace("git-repo-observer")
            )
            .await,
            Decision::Allow
        );
        // The rule applied and did not match: silence, and the floor decides.
        assert_eq!(
            ask_with(
                &actor,
                "someone-else",
                r#"Action::"store""#,
                secret,
                in_namespace("git-repo-observer")
            )
            .await,
            Decision::NotDetermined
        );
    }

    #[tokio::test]
    async fn a_name_with_no_namespace_cannot_satisfy_a_namespace_rule() {
        let actor = spawn_policy_actor().await;
        load(&actor, STORE_IN_OWN_NAMESPACE).await;
        let decision = ask_with(
            &actor,
            "git-repo-observer",
            r#"Action::"store""#,
            r#"Secret::"github""#,
            Facts::default(),
        )
        .await;
        // The rule does not apply; the store's own floor then refuses a name outside the requester's namespace.
        assert_eq!(decision, Decision::NotDetermined);
    }

    #[tokio::test]
    async fn a_namespace_whose_name_has_quotes_is_that_name_and_no_other() {
        let actor = spawn_policy_actor().await;
        load(&actor, STORE_IN_OWN_NAMESPACE).await;
        let tricky = r#"a"b\c"#;
        assert_eq!(
            ask_with(
                &actor,
                tricky,
                r#"Action::"store""#,
                r#"Secret::"s""#,
                in_namespace(tricky)
            )
            .await,
            Decision::Allow
        );
        assert_eq!(
            ask_with(
                &actor,
                "a",
                r#"Action::"store""#,
                r#"Secret::"s""#,
                in_namespace(tricky)
            )
            .await,
            Decision::NotDetermined
        );
    }

    #[tokio::test]
    async fn a_label_is_an_attribute_of_the_secret_a_rule_can_speak_of() {
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"permit (principal, action == Action::"retrieve", resource) when { resource.hasTag("kind") && resource.getTag("kind") == "token" };"#,
        )
        .await;
        let secret = r#"Secret::"s""#;
        assert_eq!(
            ask_with(
                &actor,
                "anyone",
                r#"Action::"retrieve""#,
                secret,
                labelled(&[("kind", "token")])
            )
            .await,
            Decision::Allow
        );
        assert_eq!(
            ask_with(
                &actor,
                "anyone",
                r#"Action::"retrieve""#,
                secret,
                labelled(&[("kind", "user-password")])
            )
            .await,
            Decision::NotDetermined
        );
        // A secret with no such label is not matched, and the `has` guard keeps that from being an error.
        assert_eq!(
            ask_with(
                &actor,
                "anyone",
                r#"Action::"retrieve""#,
                secret,
                Facts::default()
            )
            .await,
            Decision::NotDetermined
        );
    }

    #[tokio::test]
    async fn a_label_value_with_quotes_is_that_value_and_no_other() {
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"permit (principal, action == Action::"retrieve", resource) when { resource.hasTag("kind") && resource.getTag("kind") == "a\"b" };"#,
        )
        .await;
        assert_eq!(
            ask_with(
                &actor,
                "anyone",
                r#"Action::"retrieve""#,
                r#"Secret::"s""#,
                labelled(&[("kind", "a\"b")])
            )
            .await,
            Decision::Allow
        );
        assert_eq!(
            ask_with(
                &actor,
                "anyone",
                r#"Action::"retrieve""#,
                r#"Secret::"s""#,
                labelled(&[("kind", "a")])
            )
            .await,
            Decision::NotDetermined
        );
    }

    #[tokio::test]
    async fn a_label_never_takes_the_place_of_a_fact_the_ledger_or_the_name_gives() {
        // Labels are tags and facts are attributes: a label named like a fact is a tag, and the fact is the
        // ledger's. A requester that labels a secret `owner` with its own name does not become its owner.
        let actor = spawn_policy_actor().await;
        load(&actor, BASELINE).await;
        for key in ["owner", "namespace", "frozen"] {
            let facts = Facts {
                owner: Some("the-real-owner".to_string()),
                labels: [(key.to_string(), "someone".to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            };
            let answer = ask_with(
                &actor,
                "someone",
                r#"Action::"retrieve""#,
                r#"Secret::"s""#,
                facts,
            )
            .await;
            assert_eq!(
                answer,
                Decision::NotDetermined,
                "a label named {key} was taken"
            );
        }
    }

    const BASELINE: &str = r#"
        permit (principal, action == Action::"retrieve", resource) when { resource has owner && resource.owner == principal };
        forbid (principal, action, resource) when { resource has frozen && resource.frozen };
    "#;

    #[tokio::test]
    async fn the_owner_of_a_secret_may_retrieve_it_and_another_identity_may_not() {
        let actor = spawn_policy_actor().await;
        load(&actor, BASELINE).await;
        let secret = r#"Secret::"github-sei""#;
        assert_eq!(
            ask_with(
                &actor,
                "git-repo-observer",
                r#"Action::"retrieve""#,
                secret,
                owned_by("git-repo-observer")
            )
            .await,
            Decision::Allow
        );
        // A rule applied and found no match: silence, which a secret treats as a refusal.
        assert_eq!(
            ask_with(
                &actor,
                "someone-else",
                r#"Action::"retrieve""#,
                secret,
                owned_by("git-repo-observer")
            )
            .await,
            Decision::NotDetermined
        );
    }

    #[tokio::test]
    async fn the_owner_is_the_ledgers_word_and_a_rule_cannot_be_satisfied_without_one() {
        let actor = spawn_policy_actor().await;
        load(&actor, BASELINE).await;
        // No owner given: the rule does not apply, which a secret treats as a refusal, and never an allow.
        let decision = ask_with(
            &actor,
            "git-repo-observer",
            r#"Action::"retrieve""#,
            r#"Secret::"github-sei""#,
            Facts::default(),
        )
        .await;
        assert_eq!(decision, Decision::NotDetermined);
    }

    #[tokio::test]
    async fn an_owner_whose_name_has_quotes_is_that_name_and_no_other() {
        let actor = spawn_policy_actor().await;
        load(&actor, BASELINE).await;
        let secret = r#"Secret::"s""#;
        let tricky = r#"a"b\c"#;
        assert_eq!(
            ask_with(
                &actor,
                tricky,
                r#"Action::"retrieve""#,
                secret,
                owned_by(tricky)
            )
            .await,
            Decision::Allow
        );
        assert_eq!(
            ask_with(
                &actor,
                "a",
                r#"Action::"retrieve""#,
                secret,
                owned_by(tricky)
            )
            .await,
            Decision::NotDetermined
        );
    }

    #[tokio::test]
    async fn a_rule_can_be_limited_in_time_by_the_actors_own_clock() {
        let actor = spawn_policy_actor().await;
        load(
            &actor,
            r#"
            permit (principal == Identity::"early", action, resource) when { context.now_secs < 1000 };
            permit (principal == Identity::"late", action, resource) when { context.now_secs > 1000 };
            "#,
        )
        .await;
        let go = |who: &'static str| {
            let actor = actor.clone();
            async move {
                ask_with(
                    &actor,
                    who,
                    r#"Action::"retrieve""#,
                    r#"Secret::"s""#,
                    Facts::default(),
                )
                .await
            }
        };
        assert_eq!(go("late").await, Decision::Allow);
        assert_eq!(go("early").await, Decision::NotDetermined);
    }
}
