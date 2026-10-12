//! Signed commands: what a receiver checks before it would run one. Design: `docs/architecture/commands.md`.
//!
//! A command is a one-time instruction from an administrator, signed with a unique id and a required
//! expiry. This module is the part that needs nothing running: the checks on the document and on its time.
//! Whether the agent declares the command, whether its arguments suit it, whether it has been seen, and
//! whether the agent is draining are the agent's own, and come after.
//!
//! In order, refusing at the first failure (the first stage is [`crate::signed`]'s):
//!
//! 1. the document has exactly its two fields, the chain and the signature hold;
//! 2. its `type` is `command`;
//! 3. the payload is a command, read strictly;
//! 4. it is addressed to this agent;
//! 5. the signer holds `c:run-command`;
//! 6. the signer is not revoked;
//! 7. it is within its time.

use crate::signed::{Common, Rejection, Revocation, UpdateVerifier};
use polar_scheduler_common::document_type;
use polar_signing::{Authority, RequireCapability, SignerInfo};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The longest a command may stay valid, by default, from the time it was issued.
pub const DEFAULT_MAX_VALIDITY_SECS: u64 = 24 * 60 * 60;

/// How far clocks may disagree, by default.
pub const DEFAULT_SKEW_SECS: u64 = 60;

const MAX_ARGS: usize = 32;
const MAX_TEXT: usize = 4096;
const MAX_ID: usize = 128;
const MAX_NAME: usize = 64;

/// An argument: text or a whole number, nothing else. Nothing nested, no boolean, no float, no null.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Text(String),
    Number(i64),
}

/// When a command may run, as its document says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub issued_at: u64,
    pub not_before: Option<u64>,
    pub not_after: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRules {
    /// The longest `not_after - issued_at` that is accepted.
    pub max_validity_secs: u64,
    /// How far this agent's clock may be from the issuer's. Applied at the start of the window, and to a
    /// command that claims to be issued in the future; never at the end, where expiry is strict.
    pub skew_secs: u64,
}

impl Default for TimeRules {
    fn default() -> Self {
        Self {
            max_validity_secs: DEFAULT_MAX_VALIDITY_SECS,
            skew_secs: DEFAULT_SKEW_SECS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WindowError {
    #[error("it expires at or before the time it was issued")]
    EndsBeforeItIsIssued,
    #[error("it may not start before it is issued")]
    StartsBeforeItIsIssued,
    #[error("it may not start at or after the time it expires")]
    EmptyWindow,
    #[error("it is valid for {asked_secs} seconds, and at most {allowed_secs} are accepted")]
    TooLong { allowed_secs: u64, asked_secs: u64 },
    #[error("it says it was issued {by_secs} seconds in the future")]
    IssuedInTheFuture { by_secs: u64 },
    #[error("it may not run for another {starts_in_secs} seconds")]
    NotYet { starts_in_secs: u64 },
    #[error("it expired {ago_secs} seconds ago")]
    Expired { ago_secs: u64 },
}

/// Whether a command whose window this is may run at `now`. First what must be true of any such document
/// (its own consistency and its length), then what is true of this moment. Expiry is strict: at the
/// instant `not_after` it may still run, a second later it may not, whatever the skew, because a command
/// that has expired must not run. The start is lenient by the skew, so a clock a little behind the
/// issuer's does not refuse a command that is due.
pub fn check_window(window: &Window, rules: &TimeRules, now: u64) -> Result<(), WindowError> {
    if window.not_after <= window.issued_at {
        return Err(WindowError::EndsBeforeItIsIssued);
    }
    if let Some(start) = window.not_before {
        if start < window.issued_at {
            return Err(WindowError::StartsBeforeItIsIssued);
        }
        if start >= window.not_after {
            return Err(WindowError::EmptyWindow);
        }
    }
    let asked = window.not_after - window.issued_at;
    if asked > rules.max_validity_secs {
        return Err(WindowError::TooLong {
            allowed_secs: rules.max_validity_secs,
            asked_secs: asked,
        });
    }

    // Saturating: a clock at the very end of time must not wrap round to the start of it.
    let latest_tolerated = now.saturating_add(rules.skew_secs);
    if window.issued_at > latest_tolerated {
        return Err(WindowError::IssuedInTheFuture {
            by_secs: window.issued_at - now,
        });
    }
    let start = window.not_before.unwrap_or(window.issued_at);
    if start > latest_tolerated {
        return Err(WindowError::NotYet {
            starts_in_secs: start - now,
        });
    }
    if now > window.not_after {
        return Err(WindowError::Expired {
            ago_secs: now - window.not_after,
        });
    }
    Ok(())
}

/// A command that passed every check here. `signer` is who signed it, from their certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCommand {
    pub id: String,
    pub command: String,
    pub args: BTreeMap<String, Arg>,
    pub window: Window,
    pub signer: SignerInfo,
}

pub struct CommandVerifier {
    common: Common,
    authority: Arc<dyn Authority>,
    rules: TimeRules,
}

impl UpdateVerifier {
    /// The verifier for commands that goes with this one: the same trust, clock, revocation and agent,
    /// and its own authority, which is `c:run-command`.
    pub fn commands(&self) -> CommandVerifier {
        CommandVerifier {
            common: self.common.clone(),
            authority: Arc::new(RequireCapability::run_command()),
            rules: TimeRules::default(),
        }
    }
}

impl CommandVerifier {
    pub fn with_authority(mut self, authority: Arc<dyn Authority>) -> Self {
        self.authority = authority;
        self
    }

    pub fn with_rules(mut self, rules: TimeRules) -> Self {
        self.rules = rules;
        self
    }

    pub fn with_revocation(mut self, revocation: Arc<dyn Revocation>) -> Self {
        self.common.set_revocation(revocation);
        self
    }

    pub fn verify(&self, document: &Value) -> Result<VerifiedCommand, Rejection> {
        let (document, signer) = self.common.signed(document)?;
        self.common.expect_kind(&document, document_type::COMMAND)?;

        // The signature held, so the payload is what the signer wrote. Read it strictly.
        let command = parse(&document.payload).map_err(Rejection::Payload)?;
        self.common.check_target(command.target)?;
        self.authority
            .authorize(&signer, &document.payload)
            .map_err(Rejection::Unauthorized)?;
        self.common.check_not_revoked(&signer)?;

        let now = u64::try_from(self.common.now()).unwrap_or(0);
        check_window(&command.window, &self.rules, now).map_err(Rejection::Window)?;
        Ok(VerifiedCommand {
            id: command.id,
            command: command.command,
            args: command.args,
            window: command.window,
            signer,
        })
    }
}

struct Parsed {
    id: String,
    target: String,
    command: String,
    args: BTreeMap<String, Arg>,
    window: Window,
}

const FIELDS: [&str; 8] = [
    "type",
    "id",
    "target",
    "command",
    "args",
    "issued_at",
    "not_before",
    "not_after",
];

/// The payload of a command, strictly: the fields it has and no others, each of its kind.
fn parse(payload: &Value) -> Result<Parsed, String> {
    let object = payload
        .as_object()
        .ok_or_else(|| "the payload is an object".to_string())?;
    if let Some(extra) = object.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(format!("a command has no field `{extra}`"));
    }

    let text = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("a command has text `{name}`"))
    };
    let seconds = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("a command has a whole-number `{name}`, in seconds"))
    };

    let id = text("id")?;
    if id.is_empty()
        || id.len() > MAX_ID
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "`id` is 1 to {MAX_ID} letters, digits, dots, hyphens and underscores"
        ));
    }
    let command = text("command")?;
    if !name_is_sound(command, '-') {
        return Err(format!(
            "`command` is a lower-case name of at most {MAX_NAME} letters, digits and hyphens"
        ));
    }

    let target = object
        .get("target")
        .and_then(Value::as_object)
        .ok_or_else(|| "a command has a `target`".to_string())?;
    if let Some(extra) = target.keys().find(|k| k.as_str() != "agent") {
        return Err(format!("`target` has only `agent`, not `{extra}`"));
    }
    let agent = target
        .get("agent")
        .and_then(Value::as_str)
        .ok_or_else(|| "a command has a text `target.agent`".to_string())?;

    let mut args = BTreeMap::new();
    let given = object
        .get("args")
        .and_then(Value::as_object)
        .ok_or_else(|| "a command has an `args` object, empty if it takes none".to_string())?;
    if given.len() > MAX_ARGS {
        return Err(format!("a command has at most {MAX_ARGS} arguments"));
    }
    for (name, value) in given {
        if !name_is_sound(name, '_') {
            return Err(format!(
                "argument `{name}`: a name is a lower-case word of at most {MAX_NAME} letters, digits and underscores"
            ));
        }
        let arg = match value {
            Value::String(s) if s.len() <= MAX_TEXT => Arg::Text(s.clone()),
            Value::String(_) => {
                return Err(format!("argument `{name}` is more than {MAX_TEXT} bytes"));
            }
            Value::Number(n) => Arg::Number(
                n.as_i64()
                    .ok_or_else(|| format!("argument `{name}` is not a whole number"))?,
            ),
            _ => {
                return Err(format!(
                    "argument `{name}` is text or a whole number, nothing else"
                ));
            }
        };
        args.insert(name.clone(), arg);
    }

    let not_before = match object.get("not_before") {
        None => None,
        Some(_) => Some(seconds("not_before")?),
    };
    Ok(Parsed {
        id: id.to_string(),
        target: agent.to_string(),
        command: command.to_string(),
        args,
        window: Window {
            issued_at: seconds("issued_at")?,
            not_before,
            not_after: seconds("not_after")?,
        },
    })
}

/// A lower-case name starting with a letter, then letters, digits and `separator`.
fn name_is_sound(name: &str, separator: char) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= MAX_NAME
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == separator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_signing::testing::{self as certs, Kind, Spec, now, root, signer};
    use polar_signing::{Trust, sign};
    use serde_json::json;

    const AGENT: &str = "git-agent";
    const RULES: TimeRules = TimeRules {
        max_validity_secs: 3600,
        skew_secs: 60,
    };

    fn w(issued_at: u64, not_before: Option<u64>, not_after: u64) -> Window {
        Window {
            issued_at,
            not_before,
            not_after,
        }
    }

    // -- the window, as a pure function ---------------------------------------------------

    #[test]
    fn a_command_may_run_from_when_it_is_issued_until_it_expires() {
        let window = w(1000, None, 1600);
        for at in [1000, 1001, 1300, 1599, 1600] {
            assert_eq!(check_window(&window, &RULES, at), Ok(()), "at {at}");
        }
    }

    #[test]
    fn expiry_is_strict_and_a_second_late_is_refused_whatever_the_skew() {
        let window = w(1000, None, 1600);
        assert_eq!(
            check_window(&window, &RULES, 1601),
            Err(WindowError::Expired { ago_secs: 1 })
        );
        let generous = TimeRules {
            skew_secs: 10_000,
            ..RULES
        };
        assert_eq!(
            check_window(&window, &generous, 1601),
            Err(WindowError::Expired { ago_secs: 1 })
        );
    }

    #[test]
    fn the_start_is_lenient_by_the_skew_and_no_more() {
        let window = w(1000, Some(2000), 2600);
        // This clock is behind: 1940 + 60 reaches the start, 1939 does not.
        assert_eq!(check_window(&window, &RULES, 1940), Ok(()));
        assert_eq!(
            check_window(&window, &RULES, 1939),
            Err(WindowError::NotYet { starts_in_secs: 61 })
        );
        // With no not_before the command starts when it was issued, so the same applies to that.
        let issued = w(5000, None, 5600);
        assert_eq!(check_window(&issued, &RULES, 4940), Ok(()));
        assert_eq!(
            check_window(&issued, &RULES, 4939),
            Err(WindowError::IssuedInTheFuture { by_secs: 61 })
        );
    }

    #[test]
    fn a_command_dated_far_in_the_future_is_refused_even_if_it_has_not_expired() {
        // Issued tomorrow, valid for tomorrow: not a command to be taken today.
        let window = w(100_000, None, 100_600);
        assert!(matches!(
            check_window(&window, &RULES, 1000),
            Err(WindowError::IssuedInTheFuture { .. })
        ));
    }

    #[test]
    fn the_length_of_validity_has_a_ceiling_and_the_ceiling_itself_is_allowed() {
        assert_eq!(
            check_window(&w(1000, None, 1000 + 3600), &RULES, 1000),
            Ok(())
        );
        assert_eq!(
            check_window(&w(1000, None, 1000 + 3601), &RULES, 1000),
            Err(WindowError::TooLong {
                allowed_secs: 3600,
                asked_secs: 3601
            })
        );
    }

    #[test]
    fn a_window_that_contradicts_itself_is_refused_before_the_time_is_looked_at() {
        // Even at a moment inside every part of it.
        assert_eq!(
            check_window(&w(1000, None, 1000), &RULES, 1000),
            Err(WindowError::EndsBeforeItIsIssued)
        );
        assert_eq!(
            check_window(&w(1000, None, 900), &RULES, 950),
            Err(WindowError::EndsBeforeItIsIssued)
        );
        assert_eq!(
            check_window(&w(1000, Some(900), 1500), &RULES, 1000),
            Err(WindowError::StartsBeforeItIsIssued)
        );
        assert_eq!(
            check_window(&w(1000, Some(1500), 1500), &RULES, 1000),
            Err(WindowError::EmptyWindow)
        );
        assert_eq!(
            check_window(&w(1000, Some(1600), 1500), &RULES, 1000),
            Err(WindowError::EmptyWindow)
        );
    }

    #[test]
    fn no_arithmetic_overflows_at_the_edges_of_time() {
        let huge = w(u64::MAX - 10, None, u64::MAX);
        let _ = check_window(&huge, &RULES, 0);
        let _ = check_window(&huge, &RULES, u64::MAX);
        let _ = check_window(&w(0, None, 1), &RULES, u64::MAX);
    }

    // -- the verifier ----------------------------------------------------------------------

    fn payload() -> Value {
        // Valid from `now` for a quarter of an hour.
        let issued = now() as u64;
        json!({
            "type": "command",
            "id": "5d0c5a0e-6f43-4a5e-9a07-0a6d1d6a3b11",
            "target": {"agent": AGENT},
            "command": "backup-now",
            "args": {"destination": "nightly", "keep": 7},
            "issued_at": issued,
            "not_after": issued + 900
        })
    }

    struct Rig {
        ca: certs::Authority,
        verifier: CommandVerifier,
    }

    fn rig() -> Rig {
        let ca = root("Polar Internal CA");
        let verifier = UpdateVerifier::new(
            Trust::from_pem(std::slice::from_ref(&ca.pem)).unwrap(),
            AGENT,
        )
        .with_clock(now)
        .commands();
        Rig { ca, verifier }
    }

    fn document(rig: &Rig, capability: &'static str, payload: Value) -> Value {
        let spec = Spec {
            capabilities: vec![capability],
            not_after_year: 2090,
            ..Spec::default()
        };
        let s = signer(&rig.ca, spec);
        sign(payload, &[s.cert_pem], &s.key_pem).unwrap().to_value()
    }

    fn good(rig: &Rig) -> Value {
        document(rig, "c:run-command", payload())
    }

    #[test]
    fn a_good_command_verifies_and_is_read_into_its_parts() {
        let rig = rig();
        for kind in [Kind::P256, Kind::Ed25519] {
            let s = signer(
                &rig.ca,
                Spec {
                    kind,
                    capabilities: vec!["c:run-command"],
                    not_after_year: 2090,
                    ..Spec::default()
                },
            );
            let doc = sign(payload(), &[s.cert_pem], &s.key_pem)
                .unwrap()
                .to_value();
            let verified = rig.verifier.verify(&doc).unwrap();
            assert_eq!(verified.id, "5d0c5a0e-6f43-4a5e-9a07-0a6d1d6a3b11");
            assert_eq!(verified.command, "backup-now");
            assert_eq!(
                verified.args,
                BTreeMap::from([
                    ("destination".to_string(), Arg::Text("nightly".to_string())),
                    ("keep".to_string(), Arg::Number(7)),
                ])
            );
            assert_eq!(verified.window.not_after, now() as u64 + 900);
            assert_eq!(verified.signer.username.as_deref(), Some("alice"));
        }
    }

    #[test]
    fn any_change_after_signing_breaks_the_signature_and_an_expiry_cannot_be_extended() {
        let rig = rig();
        let doc = good(&rig);
        for edit in [
            |d: &mut Value| d["payload"]["command"] = json!("wipe-everything"),
            |d: &mut Value| d["payload"]["args"]["keep"] = json!(0),
            |d: &mut Value| d["payload"]["not_after"] = json!(u32::MAX),
            |d: &mut Value| d["payload"]["target"]["agent"] = json!("another-agent"),
            |d: &mut Value| d["payload"]["id"] = json!("another-id"),
        ] {
            let mut changed = doc.clone();
            edit(&mut changed);
            assert!(matches!(
                rig.verifier.verify(&changed),
                Err(Rejection::Signature(_))
            ));
        }
    }

    #[test]
    fn the_kind_is_checked_a_configuration_is_not_a_command() {
        let rig = rig();
        let mut config = payload();
        config["type"] = json!("configuration");
        let doc = document(&rig, "c:run-command", config);
        assert_eq!(
            rig.verifier.verify(&doc),
            Err(Rejection::WrongType {
                wanted: "command",
                got: "configuration".into()
            })
        );
        let mut none = payload();
        none.as_object_mut().unwrap().remove("type");
        assert!(matches!(
            rig.verifier.verify(&document(&rig, "c:run-command", none)),
            Err(Rejection::Payload(_))
        ));
    }

    #[test]
    fn it_must_be_for_this_agent() {
        let rig = rig();
        let mut elsewhere = payload();
        elsewhere["target"]["agent"] = json!("another-agent");
        assert_eq!(
            rig.verifier
                .verify(&document(&rig, "c:run-command", elsewhere)),
            Err(Rejection::WrongTarget {
                wanted: AGENT.into(),
                got: "another-agent".into()
            })
        );
    }

    #[test]
    fn only_a_holder_of_run_command_may_run_one() {
        let rig = rig();
        // Every other capability, and none.
        for capability in [
            "c:update-policy",
            "c:update-configuration",
            "c:grant",
            "c:revoke",
            "c:provision-account",
        ] {
            assert!(
                matches!(
                    rig.verifier.verify(&document(&rig, capability, payload())),
                    Err(Rejection::Unauthorized(_))
                ),
                "{capability}"
            );
        }
        let s = signer(
            &rig.ca,
            Spec {
                capabilities: vec![],
                not_after_year: 2090,
                ..Spec::default()
            },
        );
        let none = sign(payload(), &[s.cert_pem], &s.key_pem)
            .unwrap()
            .to_value();
        assert!(matches!(
            rig.verifier.verify(&none),
            Err(Rejection::Unauthorized(_))
        ));
    }

    #[test]
    fn a_revoked_signer_may_not() {
        struct All;
        impl Revocation for All {
            fn is_revoked(&self, _: &SignerInfo) -> bool {
                true
            }
        }
        let rig = rig();
        let revoking = UpdateVerifier::new(
            Trust::from_pem(std::slice::from_ref(&rig.ca.pem)).unwrap(),
            AGENT,
        )
        .with_clock(now)
        .commands()
        .with_revocation(Arc::new(All));
        assert!(matches!(
            revoking.verify(&good(&rig)),
            Err(Rejection::Revoked(_))
        ));
        // The same document passes the verifier that does not revoke.
        assert!(rig.verifier.verify(&good(&rig)).is_ok());
    }

    #[test]
    fn the_time_rules_apply_to_a_verified_command() {
        let rig = rig();
        let issued = now() as u64;
        type Case = (&'static str, Value, fn(&Rejection) -> bool);
        let cases: Vec<Case> = vec![
            (
                "expired",
                json!({"issued_at": issued - 2000, "not_after": issued - 1}),
                |r| matches!(r, Rejection::Window(WindowError::Expired { .. })),
            ),
            (
                "not yet",
                json!({"issued_at": issued, "not_before": issued + 600, "not_after": issued + 900}),
                |r| matches!(r, Rejection::Window(WindowError::NotYet { .. })),
            ),
            (
                "too long",
                json!({"issued_at": issued, "not_after": issued + DEFAULT_MAX_VALIDITY_SECS + 1}),
                |r| matches!(r, Rejection::Window(WindowError::TooLong { .. })),
            ),
            (
                "ends before it is issued",
                json!({"issued_at": issued, "not_after": issued}),
                |r| matches!(r, Rejection::Window(WindowError::EndsBeforeItIsIssued)),
            ),
        ];
        for (name, fields, check) in cases {
            let mut p = payload();
            for (k, v) in fields.as_object().unwrap() {
                p[k] = v.clone();
            }
            let doc = document(&rig, "c:run-command", p);
            let got = rig.verifier.verify(&doc).unwrap_err();
            assert!(check(&got), "{name}: {got:?}");
        }
    }

    #[test]
    fn the_checks_run_in_the_documented_order() {
        let rig = rig();
        let issued = now() as u64;
        // Expired AND for another agent AND not authorised: the target is named first.
        let mut p = payload();
        p["issued_at"] = json!(issued - 2000);
        p["not_after"] = json!(issued - 1);
        p["target"]["agent"] = json!("another-agent");
        assert!(matches!(
            rig.verifier.verify(&document(&rig, "c:grant", p.clone())),
            Err(Rejection::WrongTarget { .. })
        ));
        // Expired AND not authorised: the authority comes before the time.
        p["target"]["agent"] = json!(AGENT);
        assert!(matches!(
            rig.verifier.verify(&document(&rig, "c:grant", p.clone())),
            Err(Rejection::Unauthorized(_))
        ));
        // Expired, and authorised: only now is it the time.
        assert!(matches!(
            rig.verifier.verify(&document(&rig, "c:run-command", p)),
            Err(Rejection::Window(WindowError::Expired { .. }))
        ));
    }

    #[test]
    fn a_missing_or_malformed_time_is_not_a_command() {
        let rig = rig();
        for (name, edit) in [
            (
                "no not_after",
                (|p: &mut Value| {
                    p.as_object_mut().unwrap().remove("not_after");
                }) as fn(&mut Value),
            ),
            ("no issued_at", |p| {
                p.as_object_mut().unwrap().remove("issued_at");
            }),
            ("not_after as text", |p| p["not_after"] = json!("tomorrow")),
            ("not_after negative", |p| p["not_after"] = json!(-5)),
            ("not_before as text", |p| p["not_before"] = json!("soon")),
        ] {
            let mut p = payload();
            edit(&mut p);
            let doc = document(&rig, "c:run-command", p);
            match rig.verifier.verify(&doc) {
                Err(Rejection::Payload(_)) => {}
                other => panic!("{name}: {other:?}"),
            }
        }
        // And the message names what is missing.
        let mut p = payload();
        p.as_object_mut().unwrap().remove("not_after");
        match rig.verifier.verify(&document(&rig, "c:run-command", p)) {
            Err(Rejection::Payload(why)) => assert!(why.contains("not_after"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arguments_are_text_or_whole_numbers_and_nothing_else() {
        let rig = rig();
        for bad in [
            json!({"flag": true}),
            json!({"nothing": null}),
            json!({"nested": {"a": 1}}),
            json!({"list": [1, 2]}),
            json!({"huge": 18446744073709551615u64}),
            json!({"Upper": "x"}),
            json!({"has space": "x"}),
            json!({"": "x"}),
            json!({"text": "x".repeat(4097)}),
        ] {
            let mut p = payload();
            p["args"] = bad.clone();
            assert!(
                matches!(
                    rig.verifier.verify(&document(&rig, "c:run-command", p)),
                    Err(Rejection::Payload(_))
                ),
                "{bad}"
            );
        }
        // A float cannot be in a signed payload at all: the signer refuses it, which is the stronger
        // guarantee, since no verifier ever has to.
        let s = signer(
            &rig.ca,
            Spec {
                capabilities: vec!["c:run-command"],
                not_after_year: 2090,
                ..Spec::default()
            },
        );
        for floaty in [json!({"args": {"ratio": 1.5}}), json!({"not_after": 1.5})] {
            let mut p = payload();
            for (k, v) in floaty.as_object().unwrap() {
                p[k] = v.clone();
            }
            assert!(sign(p, std::slice::from_ref(&s.cert_pem), &s.key_pem).is_err());
        }

        // Not an object, and too many.
        let mut p = payload();
        p["args"] = json!([1]);
        assert!(matches!(
            rig.verifier.verify(&document(&rig, "c:run-command", p)),
            Err(Rejection::Payload(_))
        ));
        let mut p = payload();
        p["args"] = Value::Object(
            (0..33)
                .map(|i| (format!("a{i}"), json!(i)))
                .collect::<serde_json::Map<_, _>>(),
        );
        assert!(matches!(
            rig.verifier.verify(&document(&rig, "c:run-command", p)),
            Err(Rejection::Payload(_))
        ));
        // An empty set is fine, and so is the largest text.
        let mut p = payload();
        p["args"] = json!({});
        assert!(
            rig.verifier
                .verify(&document(&rig, "c:run-command", p))
                .is_ok()
        );
        let mut p = payload();
        p["args"] = json!({"text": "x".repeat(4096)});
        assert!(
            rig.verifier
                .verify(&document(&rig, "c:run-command", p))
                .is_ok()
        );
    }

    #[test]
    fn the_id_and_the_name_of_a_command_are_sound_or_it_is_refused() {
        let rig = rig();
        for (field, bad) in [
            ("id", json!("")),
            ("id", json!("has space")),
            ("id", json!("../escape")),
            ("id", json!("x".repeat(129))),
            ("id", json!(7)),
            ("command", json!("")),
            ("command", json!("Backup-Now")),
            ("command", json!("1backup")),
            ("command", json!("back_up")),
            ("command", json!("x".repeat(65))),
            ("command", json!(null)),
        ] {
            let mut p = payload();
            p[field] = bad.clone();
            assert!(
                matches!(
                    rig.verifier.verify(&document(&rig, "c:run-command", p)),
                    Err(Rejection::Payload(_))
                ),
                "{field}: {bad}"
            );
        }
    }

    #[test]
    fn a_field_a_command_does_not_have_is_refused_even_though_it_is_signed() {
        let rig = rig();
        for extra in ["version", "changes", "note"] {
            let mut p = payload();
            p[extra] = json!("x");
            match rig.verifier.verify(&document(&rig, "c:run-command", p)) {
                Err(Rejection::Payload(why)) => assert!(why.contains(extra), "{why}"),
                other => panic!("{extra}: {other:?}"),
            }
        }
        let mut p = payload();
        p["target"]["region"] = json!("eu");
        assert!(matches!(
            rig.verifier.verify(&document(&rig, "c:run-command", p)),
            Err(Rejection::Payload(_))
        ));
    }

    #[test]
    fn unsigned_or_untrusted_documents_are_refused_before_anything_is_read() {
        let rig = rig();
        assert!(matches!(
            rig.verifier.verify(&payload()),
            Err(Rejection::Layout(_))
        ));
        let rogue = root("Rogue CA");
        let s = signer(
            &rogue,
            Spec {
                capabilities: vec!["c:run-command"],
                not_after_year: 2090,
                ..Spec::default()
            },
        );
        let doc = sign(payload(), &[s.cert_pem], &s.key_pem)
            .unwrap()
            .to_value();
        assert!(matches!(
            rig.verifier.verify(&doc),
            Err(Rejection::Signature(_))
        ));
    }

    #[test]
    fn the_rules_can_be_changed_for_a_deployment() {
        let rig = rig();
        let doc = good(&rig);
        // A deployment that allows only five minutes refuses the fifteen this one asked for.
        let strict = rig.verifier.with_rules(TimeRules {
            max_validity_secs: 300,
            skew_secs: 60,
        });
        assert!(matches!(
            strict.verify(&doc),
            Err(Rejection::Window(WindowError::TooLong {
                allowed_secs: 300,
                ..
            }))
        ));
    }
}
