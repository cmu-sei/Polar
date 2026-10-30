//! The `credentials` runtime section: the credentials a build may be cloned with, each only for the hosts it
//! names, and which a repository's mapping asks for by name.
//!
//! Adapted from the git observer's section of the same name (the fork's `git/observe/src/credentials.rs`), which
//! has the same job for its own clones. An entry carries its secret as ciphertext, written by someone who holds
//! this agent's escrow public key and bound to this agent, this section, the version and the entry's name. Here
//! the shape is judged without opening anything, and each entry is handed to the CryptoActor, which opens it,
//! keeps it with its binding and escrows it. This agent holds the plaintext only for the moment it takes to put
//! it in a build's Secret, and a problem is reported by entry and field, never by value.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use orchestrator_core::types::GitSecret;
use polar_agent_builder::runtime::SectionAuthority;
use polar_agent_builder::{CryptoClient, RuntimeSection};
use polar_crypto::registry::{SecretBinding, SecretKind, UseRefusal, pattern_problem};
use polar_crypto::CryptoError;
use polar_escrow::EscrowedSecret;
use serde_json::Value;
use tracing::{info, warn};

/// What the section is called in a document.
pub const SECTION_NAME: &str = "credentials";
/// Most entries one version may carry.
pub const MAX_ENTRIES: usize = 256;
/// Longest user name.
pub const MAX_USERNAME: usize = 128;

pub static CREDENTIALS: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Runtime,
    name: SECTION_NAME,
    doc: "Credentials a build may be cloned with, each only for the hosts it names. A repository's mapping asks for one by name. Replaced whole by each version, so an entry left out is withdrawn.",
    body: r#"{"credentials": {"list_of": {"name": {"type": "text of lowercase letters, digits and hyphens", "required": true, "doc": "what a mapping calls the credential"}, "host_pattern": {"type": "host pattern", "required": true, "doc": "the only hosts this credential is ever used for: a host name, or *. and a domain"}, "kind": {"type": "token | user-password", "required": true}, "username": {"type": "text", "required": false, "doc": "required for a user-password; for a token the default is oauth2"}, "secret": {"type": "ciphertext", "required": true, "secret": true, "doc": "the secret itself. In the signed document it is base64 of an EscrowedSecret for this agent, bound to build-orchestrator|credentials|<version>|<name>|secret; polar-sign makes that from what you type"}}}}"#,
    validate,
};

/// One entry, as the document gives it. The secret is still the base64 of the ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub host_pattern: String,
    pub kind: SecretKind,
    pub username: Option<String>,
    pub secret: String,
}

fn validate(body: &Value) -> Result<(), Vec<String>> {
    parse(body).map(|_| ())
}

/// The entries of a body, or every problem with it. Judges the shape and whether the secret is, by its bytes,
/// an escrowed secret; nothing is opened.
pub fn parse(body: &Value) -> Result<Vec<Entry>, Vec<String>> {
    let Some(object) = body.as_object() else {
        return Err(vec!["the body must be an object".to_string()]);
    };
    let mut problems: Vec<String> = object
        .keys()
        .filter(|k| k.as_str() != "credentials")
        .map(|k| format!("`{k}` is not a field of this section"))
        .collect();
    let Some(list) = object.get("credentials").and_then(Value::as_array) else {
        problems.push("`credentials` must be a list of entries".to_string());
        return Err(problems);
    };
    if list.len() > MAX_ENTRIES {
        problems.push(format!(
            "`credentials` has {} entries, and at most {MAX_ENTRIES} are taken",
            list.len()
        ));
    }
    let mut entries: Vec<Entry> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        match entry(i, item) {
            Ok(entry) => {
                if entries.iter().any(|e| e.name == entry.name) {
                    problems.push(format!("entry {i}: the name `{}` is used more than once", entry.name));
                }
                entries.push(entry);
            }
            Err(mut more) => problems.append(&mut more),
        }
    }
    if problems.is_empty() { Ok(entries) } else { Err(problems) }
}

fn entry(i: usize, item: &Value) -> Result<Entry, Vec<String>> {
    let Some(object) = item.as_object() else {
        return Err(vec![format!("entry {i}: must be an object")]);
    };
    let mut problems = Vec::new();
    for key in object.keys() {
        if !["name", "host_pattern", "kind", "username", "secret"].contains(&key.as_str()) {
            problems.push(format!("entry {i}: `{key}` is not a field of an entry"));
        }
    }
    let text = |key: &str| object.get(key).and_then(Value::as_str);

    let name = text("name").unwrap_or_default().to_string();
    match text("name") {
        None => problems.push(format!("entry {i}: `name` must be text")),
        Some(n) => {
            if let Some(why) = git_agent_common::remote::credential_name_problem(n) {
                problems.push(format!("entry {i}: `name` {why}"));
            }
        }
    }
    // The entry is named by its name from here on, and by its position when it has none.
    let label = if name.is_empty() { format!("entry {i}") } else { format!("entry `{name}`") };

    let host_pattern = text("host_pattern").unwrap_or_default().to_string();
    match text("host_pattern") {
        None => problems.push(format!("{label}: `host_pattern` must be text")),
        Some(p) => {
            if let Some(why) = pattern_problem(p) {
                problems.push(format!("{label}: `host_pattern`: {why}"));
            }
        }
    }

    let kind = match object.get("kind").map(|k| serde_json::from_value(k.clone())) {
        Some(Ok(kind)) => Some(kind),
        _ => {
            problems.push(format!("{label}: `kind` must be `token` or `user-password`"));
            None
        }
    };

    let username = match object.get("username") {
        None | Some(Value::Null) => None,
        Some(Value::String(u)) if !u.is_empty() && u.len() <= MAX_USERNAME && !u.chars().any(char::is_control) => {
            Some(u.clone())
        }
        Some(_) => {
            problems.push(format!(
                "{label}: `username` must be 1 to {MAX_USERNAME} characters, none a control character"
            ));
            None
        }
    };
    if kind == Some(SecretKind::UserPassword) && username.is_none() {
        problems.push(format!("{label}: a `user-password` credential needs a `username`"));
    }

    let secret = text("secret").unwrap_or_default().to_string();
    match text("secret") {
        None => problems.push(format!("{label}: `secret` must be text")),
        Some(s) => {
            if let Some(why) = secret_problem(s) {
                problems.push(format!("{label}: `secret` {why}"));
            }
        }
    }

    match (problems.is_empty(), kind) {
        (true, Some(kind)) => Ok(Entry { name, host_pattern, kind, username, secret }),
        _ => Err(problems),
    }
}

/// Why a secret field is not the base64 of an escrowed secret, without saying what it holds.
fn secret_problem(secret: &str) -> Option<&'static str> {
    match EscrowedSecret::from_document_text(secret) {
        Ok(_) => None,
        Err(polar_escrow::DocumentTextError::NotBase64) => Some("is not standard base64"),
        Err(polar_escrow::DocumentTextError::Empty) => Some("is empty"),
        Err(polar_escrow::DocumentTextError::NotASecret) => Some("is not an escrowed secret"),
    }
}

/// What an entry was bound to when it was encrypted (docs/architecture/signed-configuration.md, "Binding"): the
/// one definition the author's tool uses too (`polar_escrow::binding_context`).
pub fn context(agent: &str, version: u64, name: &str) -> Result<Vec<u8>, polar_escrow::ContextError> {
    polar_escrow::binding_context(agent, SECTION_NAME, version, name, "secret")
}

/// What happened to a version of the section.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Registered with the CryptoActor.
    pub registered: Vec<String>,
    /// Refused or failed, with the reason (which never holds a secret).
    pub failed: Vec<(String, String)>,
    /// Withdrawn: registered before, and not in this version.
    pub withdrawn: Vec<String>,
}

/// Hands a version of the section to the CryptoActor: each entry is registered, then every name this version
/// does not list is withdrawn. An entry that fails is reported by name and does not stop the rest. The
/// withdrawal is for everything the version leaves out whether or not its own entries registered, since a
/// version is the whole section.
pub async fn apply(
    crypto: &dyn CryptoClient,
    agent: &str,
    version: u64,
    entries: &[Entry],
) -> Result<Applied, CryptoError> {
    let mut applied = Applied::default();
    for entry in entries {
        let secret = match STANDARD.decode(&entry.secret) {
            Ok(bytes) => bytes,
            Err(_) => {
                applied.failed.push((entry.name.clone(), "the secret is not base64".to_string()));
                continue;
            }
        };
        let context = match context(agent, version, &entry.name) {
            Ok(context) => context,
            Err(e) => {
                warn!(credential = %entry.name, "could not register the credential: {e}");
                applied.failed.push((entry.name.clone(), e.to_string()));
                continue;
            }
        };
        let binding = SecretBinding {
            host_pattern: entry.host_pattern.clone(),
            kind: entry.kind,
            username: entry.username.clone(),
        };
        match crypto
            .register_secret(entry.name.clone(), SECTION_NAME.to_string(), secret, context, binding)
            .await
        {
            Ok(()) => applied.registered.push(entry.name.clone()),
            Err(e) => {
                warn!(credential = %entry.name, "could not register the credential: {e}");
                applied.failed.push((entry.name.clone(), e.to_string()));
            }
        }
    }
    let keep = entries.iter().map(|e| e.name.clone()).collect();
    applied.withdrawn = crypto.replace_section(SECTION_NAME.to_string(), keep).await?;
    info!(
        version,
        registered = applied.registered.len(),
        failed = applied.failed.len(),
        withdrawn = applied.withdrawn.len(),
        "credentials section applied"
    );
    Ok(applied)
}

/// What a build gets for the credentials its repository's mapping names.
#[derive(Debug)]
pub enum Resolution {
    /// The mapping names none: a public repository.
    Public,
    /// The first named credential that is bound to the repository's host.
    Ready(GitSecret),
    /// The build must not start, and why. The reason names credentials, never their values.
    Refused(String),
}

/// Asks the CryptoActor for the credentials a mapping names, for the repository's host, in the order named.
/// The CryptoActor is what refuses a host the credential is not bound to; this only asks.
pub async fn resolve(crypto: &dyn CryptoClient, names: &[String], repo_url: &str) -> Resolution {
    if names.is_empty() {
        return Resolution::Public;
    }
    let host = match git_agent_common::remote::https_host(repo_url) {
        Ok(host) => host,
        Err(why) => return Resolution::Refused(why),
    };
    let mut reasons: Vec<String> = Vec::new();
    for name in names {
        match crypto.git_credential(name.clone(), host.clone()).await {
            Ok(credential) => {
                return match String::from_utf8(credential.secret.to_vec()) {
                    Ok(password) => Resolution::Ready(GitSecret { username: credential.username, password }),
                    Err(_) => Resolution::Refused(format!("the credential `{name}` is not text")),
                };
            }
            Err(CryptoError::SecretNotRegistered(_)) => {
                reasons.push(format!("the credential `{name}` has not arrived"));
            }
            Err(CryptoError::Use(UseRefusal::HostNotBound)) => {
                reasons.push(format!("the credential `{name}` is not bound to this repository's host"));
            }
            Err(other) => reasons.push(format!("the credential `{name}` cannot be used: {other}")),
        }
    }
    Resolution::Refused(reasons.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use polar_crypto::registry::GitCredential;
    use polar_crypto::Zeroizing;
    use serde_json::json;
    use std::sync::Mutex;

    /// A CryptoActor that records what it is asked and answers as it is told.
    #[derive(Default)]
    struct Fake {
        registered: Mutex<Vec<(String, String, Vec<u8>, Vec<u8>, SecretBinding)>>,
        replaced: Mutex<Vec<(String, Vec<String>)>>,
        refuse: Vec<&'static str>,
        withdraw: Vec<String>,
        answers: Vec<(&'static str, Answer)>,
        asked: Mutex<Vec<(String, String)>>,
    }

    #[derive(Clone)]
    enum Answer {
        Found { username: &'static str, secret: &'static [u8] },
        NotRegistered,
        HostNotBound,
    }

    #[async_trait]
    impl CryptoClient for Fake {
        async fn store_secret(&self, _: String, _: Vec<u8>) -> Result<(), CryptoError> {
            unreachable!()
        }
        async fn encrypt_for_recipient(&self, _: String, _: Vec<u8>) -> Result<Vec<u8>, CryptoError> {
            unreachable!()
        }
        async fn retrieve_secret(&self, _: String) -> Result<Option<Zeroizing<Vec<u8>>>, CryptoError> {
            unreachable!()
        }
        async fn register_secret(
            &self,
            name: String,
            section: String,
            secret: Vec<u8>,
            context: Vec<u8>,
            binding: SecretBinding,
        ) -> Result<(), CryptoError> {
            if self.refuse.contains(&name.as_str()) {
                return Err(CryptoError::SecretNotRegistered(name));
            }
            self.registered.lock().unwrap().push((name, section, secret, context, binding));
            Ok(())
        }
        async fn load_secret(&self, _: String, _: String) -> Result<bool, CryptoError> {
            unreachable!()
        }
        async fn replace_section(&self, section: String, keep: Vec<String>) -> Result<Vec<String>, CryptoError> {
            self.replaced.lock().unwrap().push((section, keep));
            Ok(self.withdraw.clone())
        }
        async fn git_credential(&self, name: String, host: String) -> Result<GitCredential, CryptoError> {
            self.asked.lock().unwrap().push((name.clone(), host));
            match self.answers.iter().find(|(n, _)| *n == name).map(|(_, a)| a.clone()) {
                Some(Answer::Found { username, secret }) => Ok(GitCredential {
                    username: username.to_string(),
                    secret: Zeroizing::new(secret.to_vec()),
                }),
                Some(Answer::HostNotBound) => Err(CryptoError::Use(UseRefusal::HostNotBound)),
                Some(Answer::NotRegistered) | None => Err(CryptoError::SecretNotRegistered(name)),
            }
        }
    }

    /// The public half of an RSA key made for these tests and thrown away: sealing needs only this.
    const PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEArTjeZCvDj3d3+r5QCNvV\nuLTY4RANe0dgw6FCAn/caXPoVISGKzbfC2tivMDnkrzBy+XlgZbhJIq5sYQ6F5TQ\nTv9DUPDUjjOSwzuxiHe4kvhCVfOJIpHrWueOMXO4XC7loBPG+z0h40eAoivbw+t2\nl8WSjyyEcX7xMnLMEYTvAJyXlPxcZ9czuZzAFFj8b45wBVSgXDcH8WY8NsJ0LFSY\npZCxv/FuyyQ7uZZfc6TizSecuiZGIzC0OenqmG0HuS8qkMwv4/k4TfN2SlgszyIU\n2DCkePKn58q4Cw+7tBHO1MHem0xzA3vYqJSwpp48xsb24uNLyTRhUPceWup52yZV\noQIDAQAB\n-----END PUBLIC KEY-----\n";

    /// A real escrowed secret, base64 as a document carries it.
    fn escrowed() -> String {
        EscrowedSecret::encrypt_bound(b"a-token", PUBLIC_PEM, b"context").unwrap().to_document_text().unwrap()
    }

    fn entry_json(name: &str) -> Value {
        json!({"name": name, "host_pattern": "git.example.org", "kind": "token", "secret": escrowed()})
    }

    #[test]
    fn an_entry_reads_and_its_secret_is_not_opened() {
        let entries = parse(&json!({"credentials": [entry_json("git-a")]})).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "git-a");
        assert_eq!(entries[0].kind, SecretKind::Token);
    }

    #[test]
    fn every_problem_is_named_and_no_value_is_echoed() {
        let problems = parse(&json!({"credentials": [
            {"name": "BAD NAME", "host_pattern": "git.example.org", "kind": "token", "secret": "not-a-secret-xyz"},
            {"name": "git-b", "host_pattern": "git.example.org", "kind": "password", "secret": escrowed()},
            {"name": "git-c", "host_pattern": "git.example.org", "kind": "user-password", "secret": escrowed()},
            entry_json("git-d"), entry_json("git-d"),
        ], "other": 1}))
        .unwrap_err();
        for expected in ["`other`", "entry 0", "entry `git-b`: `kind`", "entry `git-c`: a `user-password`", "more than once"] {
            assert!(problems.iter().any(|p| p.contains(expected)), "{expected} in {problems:?}");
        }
        assert!(!problems.iter().any(|p| p.contains("not-a-secret-xyz")), "a secret field must never be echoed");
    }

    #[tokio::test]
    async fn apply_registers_each_entry_bound_to_this_agent_and_withdraws_the_rest() {
        let fake = Fake { withdraw: vec!["old".into()], ..Default::default() };
        let entries = parse(&json!({"credentials": [entry_json("git-a"), entry_json("git-b")]})).unwrap();
        let applied = apply(&fake, "build-orchestrator", 7, &entries).await.unwrap();

        assert_eq!(applied.registered, ["git-a", "git-b"]);
        assert_eq!(applied.withdrawn, ["old"]);
        let registered = fake.registered.lock().unwrap();
        assert_eq!(registered[0].1, SECTION_NAME);
        assert_eq!(registered[0].3, context("build-orchestrator", 7, "git-a").unwrap(), "bound to this agent and version");
        assert_eq!(fake.replaced.lock().unwrap()[0], (SECTION_NAME.to_string(), vec!["git-a".into(), "git-b".into()]));
    }

    #[tokio::test]
    async fn an_entry_that_fails_does_not_stop_the_rest() {
        let fake = Fake { refuse: vec!["git-a"], ..Default::default() };
        let entries = parse(&json!({"credentials": [entry_json("git-a"), entry_json("git-b")]})).unwrap();
        let applied = apply(&fake, "build-orchestrator", 1, &entries).await.unwrap();
        assert_eq!(applied.registered, ["git-b"]);
        assert_eq!(applied.failed.len(), 1);
        assert_eq!(applied.failed[0].0, "git-a");
    }

    #[tokio::test]
    async fn a_mapping_that_names_nothing_gets_nothing_and_asks_nobody() {
        let fake = Fake::default();
        assert!(matches!(resolve(&fake, &[], "https://git.example.org/a.git").await, Resolution::Public));
        assert!(fake.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_named_credential_is_asked_for_by_the_repositorys_host() {
        let fake = Fake {
            answers: vec![("git-a", Answer::Found { username: "deploy", secret: b"tok" })],
            ..Default::default()
        };
        let got = resolve(&fake, &["git-a".into()], "https://git.example.org:8443/a/b.git").await;
        match got {
            Resolution::Ready(s) => assert_eq!((s.username.as_str(), s.password.as_str()), ("deploy", "tok")),
            other => panic!("{other:?}"),
        }
        assert_eq!(fake.asked.lock().unwrap()[0], ("git-a".to_string(), "git.example.org".to_string()));
    }

    #[tokio::test]
    async fn a_credential_that_has_not_arrived_or_is_for_another_host_refuses_the_build_and_names_it() {
        let fake = Fake {
            answers: vec![("git-a", Answer::NotRegistered), ("git-b", Answer::HostNotBound)],
            ..Default::default()
        };
        match resolve(&fake, &["git-a".into(), "git-b".into()], "https://git.example.org/a.git").await {
            Resolution::Refused(why) => {
                assert!(why.contains("`git-a` has not arrived"), "{why}");
                assert!(why.contains("`git-b` is not bound"), "{why}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_second_named_credential_is_tried_when_the_first_is_not_for_this_host() {
        let fake = Fake {
            answers: vec![
                ("git-a", Answer::HostNotBound),
                ("git-b", Answer::Found { username: "u", secret: b"s" }),
            ],
            ..Default::default()
        };
        assert!(matches!(
            resolve(&fake, &["git-a".into(), "git-b".into()], "https://git.example.org/a.git").await,
            Resolution::Ready(_)
        ));
    }

    #[tokio::test]
    async fn a_url_a_credential_may_not_be_used_for_is_refused_before_asking() {
        let fake = Fake::default();
        assert!(matches!(
            resolve(&fake, &["git-a".into()], "http://git.example.org/a.git").await,
            Resolution::Refused(_)
        ));
        assert!(fake.asked.lock().unwrap().is_empty());
    }
}
