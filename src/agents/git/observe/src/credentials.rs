//! The observer's `credentials` runtime section: the credentials it may use, each only for the hosts it names
//! (docs/architecture/git-controller.md, "A credential").
//!
//! An entry carries its secret as ciphertext, written by someone who holds the observer's escrow public key and
//! bound to this agent, this section, the version and the entry's name. Here the shape is judged without
//! opening anything, and each entry is handed to the CryptoActor, which opens it, keeps it with its binding and
//! escrows it. This agent never has the plaintext, and a problem is reported by entry and field, never by value.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use credentials_agent_common::escrow_secret::EscrowedSecret;
use polar_agent_builder::runtime::SectionAuthority;
use polar_agent_builder::{CryptoClient, RuntimeSection};
use polar_crypto::registry::{SecretBinding, SecretKind, pattern_problem};
use serde_json::Value;
use tracing::{info, warn};

/// What the section is called in a document.
pub const SECTION_NAME: &str = "credentials";
/// Most entries one version may carry.
pub const MAX_ENTRIES: usize = 256;
/// Longest entry name.
pub const MAX_NAME: usize = 64;
/// Longest user name.
pub const MAX_USERNAME: usize = 128;

pub static CREDENTIALS: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Runtime,
    name: SECTION_NAME,
    doc: "Credentials the observer may use, each only for the hosts it names. Replaced whole by each version, so an entry left out is withdrawn.",
    body: r#"{"credentials": {"list_of": {"name": {"type": "text of lowercase letters, digits and hyphens", "required": true, "doc": "what a repository calls the credential"}, "host_pattern": {"type": "host pattern", "required": true, "doc": "the only hosts this credential is ever sent to: a host name, or *. and a domain"}, "kind": {"type": "token | user-password", "required": true}, "username": {"type": "text", "required": false, "doc": "required for a user-password; for a token the default is oauth2"}, "secret": {"type": "ciphertext", "required": true, "secret": true, "doc": "base64 of an EscrowedSecret for this agent, bound to git-repo-observer|credentials|<version>|<name>|secret"}}}}"#,
    validate: validate,
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

fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.len() > MAX_NAME {
        Some("must be 1 to 64 characters")
    } else if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        Some("may have only lowercase letters, digits and hyphens")
    } else {
        None
    }
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
                    problems.push(format!(
                        "entry {i}: the name `{}` is used more than once",
                        entry.name
                    ));
                }
                entries.push(entry);
            }
            Err(mut more) => problems.append(&mut more),
        }
    }
    if problems.is_empty() {
        Ok(entries)
    } else {
        Err(problems)
    }
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
            if let Some(why) = name_problem(n) {
                problems.push(format!("entry {i}: `name` {why}"));
            }
        }
    }
    // The entry is named by its name from here on, and by its position when it has none.
    let label = if name.is_empty() {
        format!("entry {i}")
    } else {
        format!("entry `{name}`")
    };

    let host_pattern = text("host_pattern").unwrap_or_default().to_string();
    match text("host_pattern") {
        None => problems.push(format!("{label}: `host_pattern` must be text")),
        Some(p) => {
            if let Some(why) = pattern_problem(p) {
                problems.push(format!("{label}: `host_pattern`: {why}"));
            }
        }
    }

    let kind = match object
        .get("kind")
        .map(|k| serde_json::from_value(k.clone()))
    {
        Some(Ok(kind)) => Some(kind),
        _ => {
            problems.push(format!(
                "{label}: `kind` must be `token` or `user-password`"
            ));
            None
        }
    };

    let username = match object.get("username") {
        None | Some(Value::Null) => None,
        Some(Value::String(u))
            if !u.is_empty() && u.len() <= MAX_USERNAME && !u.chars().any(char::is_control) =>
        {
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
        problems.push(format!(
            "{label}: a `user-password` credential needs a `username`"
        ));
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
        (true, Some(kind)) => Ok(Entry {
            name,
            host_pattern,
            kind,
            username,
            secret,
        }),
        _ => Err(problems),
    }
}

/// Why a secret field is not the base64 of an escrowed secret, without saying what it holds.
fn secret_problem(secret: &str) -> Option<&'static str> {
    let Ok(bytes) = STANDARD.decode(secret) else {
        return Some("is not standard base64");
    };
    if bytes.is_empty() {
        return Some("is empty");
    }
    match rkyv::from_bytes::<EscrowedSecret, rkyv::rancor::Error>(&bytes) {
        Ok(_) => None,
        Err(_) => Some("is not an escrowed secret"),
    }
}

/// What an entry was bound to when it was encrypted: the agent, the section, the version, the entry's name and
/// the field, joined by `|` (docs/architecture/signed-configuration.md, "Binding").
pub fn context(agent: &str, version: u64, name: &str) -> Vec<u8> {
    format!("{agent}|{SECTION_NAME}|{version}|{name}|secret").into_bytes()
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
) -> Result<Applied, polar_crypto::CryptoError> {
    let mut applied = Applied::default();
    for entry in entries {
        let secret = match STANDARD.decode(&entry.secret) {
            Ok(bytes) => bytes,
            Err(_) => {
                applied
                    .failed
                    .push((entry.name.clone(), "the secret is not base64".to_string()));
                continue;
            }
        };
        let binding = SecretBinding {
            host_pattern: entry.host_pattern.clone(),
            kind: entry.kind,
            username: entry.username.clone(),
        };
        match crypto
            .register_secret(
                entry.name.clone(),
                SECTION_NAME.to_string(),
                secret,
                context(agent, version, &entry.name),
                binding,
            )
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
    applied.withdrawn = crypto
        .replace_section(SECTION_NAME.to_string(), keep)
        .await?;
    info!(
        version,
        registered = applied.registered.len(),
        failed = applied.failed.len(),
        withdrawn = applied.withdrawn.len(),
        "credentials section applied"
    );
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use polar_crypto::CryptoError;
    use polar_crypto::registry::GitCredential;
    use serde_json::json;
    use std::sync::Mutex;

    /// The public half of one key, made once: generating an RSA key is slow.
    fn public_pem() -> &'static str {
        use rsa::pkcs8::EncodePublicKey;
        static PEM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        PEM.get_or_init(|| {
            let mut rng = rand::rng();
            let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
            key.to_public_key()
                .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap()
        })
    }

    /// A real escrowed secret, base64 as a document carries it.
    fn ciphertext(plaintext: &[u8], context: &[u8]) -> String {
        let escrowed = EscrowedSecret::encrypt_bound(plaintext, public_pem(), context).unwrap();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&escrowed).unwrap();
        STANDARD.encode(bytes.as_slice())
    }

    fn good_entry(name: &str) -> Value {
        json!({
            "name": name,
            "host_pattern": "github.com",
            "kind": "token",
            "username": "oauth2",
            "secret": ciphertext(b"ghp-token", &context("git-repo-observer", 1, name)),
        })
    }

    fn problems_of(body: Value) -> Vec<String> {
        parse(&body).expect_err("the body should be refused")
    }

    fn with(entry: Value, key: &str, value: Value) -> Value {
        let mut entry = entry;
        entry[key] = value;
        entry
    }

    #[test]
    fn the_section_is_declared_soundly() {
        assert!(polar_agent_builder::runtime::check_declarations(&[&CREDENTIALS]).is_empty());
    }

    #[test]
    fn a_sound_body_gives_its_entries() {
        let entries = parse(&json!({"credentials": [good_entry("github-sei"), {
            "name": "gitlab", "host_pattern": "*.gitlab.example.org", "kind": "user-password",
            "username": "svc", "secret": ciphertext(b"pw", b"x")}]}))
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "github-sei");
        assert_eq!(entries[0].kind, SecretKind::Token);
        assert_eq!(entries[0].username.as_deref(), Some("oauth2"));
        assert_eq!(entries[1].kind, SecretKind::UserPassword);
        assert_eq!(entries[1].host_pattern, "*.gitlab.example.org");
    }

    #[test]
    fn an_empty_list_is_a_section_that_withdraws_everything() {
        assert_eq!(parse(&json!({"credentials": []})).unwrap(), Vec::new());
    }

    #[test]
    fn a_username_is_optional_for_a_token() {
        let mut entry = good_entry("a");
        entry.as_object_mut().unwrap().remove("username");
        assert_eq!(
            parse(&json!({"credentials": [entry]})).unwrap()[0].username,
            None
        );
    }

    #[test]
    fn the_body_itself_must_be_the_right_shape() {
        assert!(problems_of(json!([])).iter().any(|p| p.contains("object")));
        assert!(
            problems_of(json!({}))
                .iter()
                .any(|p| p.contains("`credentials` must be a list"))
        );
        assert!(
            problems_of(json!({"credentials": {}}))
                .iter()
                .any(|p| p.contains("list"))
        );
        let strange = problems_of(json!({"credentials": [], "extra": 1}));
        assert!(strange.iter().any(|p| p.contains("`extra`")), "{strange:?}");
        let many: Vec<Value> = (0..=MAX_ENTRIES)
            .map(|i| good_entry(&format!("c{i}")))
            .collect();
        assert!(
            problems_of(json!({"credentials": many}))
                .iter()
                .any(|p| p.contains("at most"))
        );
    }

    #[test]
    fn each_bad_field_is_reported_by_entry_and_field() {
        let cases: Vec<(Value, &str)> = vec![
            (
                with(good_entry("a"), "name", json!("Upper")),
                "`name` may have only",
            ),
            (
                with(good_entry("a"), "name", json!("")),
                "`name` must be 1 to 64",
            ),
            (
                with(good_entry("a"), "name", json!("a".repeat(65))),
                "`name` must be 1 to 64",
            ),
            (
                with(good_entry("a"), "name", json!(7)),
                "`name` must be text",
            ),
            (
                with(good_entry("a"), "host_pattern", json!("*")),
                "`host_pattern`",
            ),
            (
                with(good_entry("a"), "host_pattern", json!("github.com:443")),
                "`host_pattern`",
            ),
            (
                with(good_entry("a"), "host_pattern", json!(3)),
                "`host_pattern` must be text",
            ),
            (
                with(good_entry("a"), "kind", json!("ssh")),
                "`kind` must be",
            ),
            (with(good_entry("a"), "kind", json!(null)), "`kind` must be"),
            (with(good_entry("a"), "username", json!("")), "`username`"),
            (
                with(good_entry("a"), "username", json!("a\nb")),
                "`username`",
            ),
            (
                with(good_entry("a"), "secret", json!("***")),
                "`secret` is not standard base64",
            ),
            (
                with(good_entry("a"), "secret", json!("")),
                "`secret` is empty",
            ),
            (
                with(good_entry("a"), "secret", json!(5)),
                "`secret` must be text",
            ),
            (
                with(good_entry("a"), "stray", json!(1)),
                "`stray` is not a field of an entry",
            ),
        ];
        for (entry, expected) in cases {
            let problems = problems_of(json!({"credentials": [entry.clone()]}));
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{entry}: wanted `{expected}`, got {problems:?}"
            );
        }
        assert!(
            problems_of(json!({"credentials": [3]}))
                .iter()
                .any(|p| p.contains("entry 0: must be an object"))
        );
    }

    #[test]
    fn a_problem_names_the_entry_by_name_when_it_has_one() {
        let problems = problems_of(
            json!({"credentials": [with(good_entry("github-sei"), "kind", json!("ssh"))]}),
        );
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("entry `github-sei`:")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_user_password_needs_a_username() {
        let mut entry = with(good_entry("a"), "kind", json!("user-password"));
        entry.as_object_mut().unwrap().remove("username");
        let problems = problems_of(json!({"credentials": [entry]}));
        assert!(
            problems.iter().any(|p| p.contains("needs a `username`")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_name_used_twice_is_refused() {
        let problems = problems_of(json!({"credentials": [good_entry("a"), good_entry("a")]}));
        assert!(
            problems.iter().any(|p| p.contains("used more than once")),
            "{problems:?}"
        );
    }

    #[test]
    fn base64_that_is_not_an_escrowed_secret_is_refused() {
        let junk = STANDARD.encode(b"just some bytes that are not an escrowed secret at all");
        let problems =
            problems_of(json!({"credentials": [with(good_entry("a"), "secret", json!(junk))]}));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("is not an escrowed secret")),
            "{problems:?}"
        );
    }

    #[test]
    fn every_problem_of_a_body_is_reported_at_once() {
        let body = json!({"credentials": [
            with(good_entry("a"), "kind", json!("ssh")),
            with(good_entry("b"), "host_pattern", json!("*")),
        ], "extra": true});
        assert!(problems_of(body).len() >= 3);
    }

    #[test]
    fn a_problem_never_repeats_what_a_secret_field_held() {
        let canary = "SECRET-CANARY-VALUE";
        for secret in [canary.to_string(), STANDARD.encode(canary)] {
            let problems = problems_of(
                json!({"credentials": [with(good_entry("a"), "secret", json!(secret))]}),
            );
            for p in &problems {
                assert!(!p.contains(canary) && !p.contains(&secret), "{p}");
            }
        }
        // nor a name or username that was refused
        let problems = problems_of(
            json!({"credentials": [with(good_entry("a"), "username", json!("us\ner-CANARY"))]}),
        );
        assert!(
            problems.iter().all(|p| !p.contains("CANARY")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_validator_the_builder_runs_is_the_parser() {
        assert!((CREDENTIALS.validate)(&json!({"credentials": [good_entry("a")]})).is_ok());
        assert!(
            (CREDENTIALS.validate)(
                &json!({"credentials": [with(good_entry("a"), "kind", json!("x"))]})
            )
            .is_err()
        );
    }

    #[test]
    fn the_context_is_agent_section_version_name_and_field() {
        assert_eq!(
            context("git-repo-observer", 7, "github-sei"),
            b"git-repo-observer|credentials|7|github-sei|secret".to_vec()
        );
    }

    /// Records what the observer asks of the CryptoActor.
    #[derive(Default)]
    struct Recorder {
        registered: Mutex<Vec<(String, String, Vec<u8>, Vec<u8>, SecretBinding)>>,
        replaced: Mutex<Vec<(String, Vec<String>)>>,
        refuse: Vec<&'static str>,
        withdraw: Vec<String>,
        replace_fails: bool,
    }

    #[async_trait]
    impl CryptoClient for Recorder {
        async fn store_secret(&self, _: String, _: Vec<u8>) -> Result<(), CryptoError> {
            unreachable!()
        }
        async fn decrypt_own_message(&self, _: Vec<u8>) -> Result<Vec<u8>, CryptoError> {
            unreachable!()
        }
        async fn encrypt_for_recipient(
            &self,
            _: String,
            _: Vec<u8>,
        ) -> Result<Vec<u8>, CryptoError> {
            unreachable!()
        }
        async fn ensure_bootstrapped(&self) -> Result<(), CryptoError> {
            unreachable!()
        }
        async fn retrieve_secret(
            &self,
            _: String,
        ) -> Result<Option<polar_crypto::Zeroizing<Vec<u8>>>, CryptoError> {
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
                return Err(CryptoError::DecryptFailed("does not open".to_string()));
            }
            self.registered
                .lock()
                .unwrap()
                .push((name, section, secret, context, binding));
            Ok(())
        }
        async fn load_secret(&self, _: String, _: String) -> Result<bool, CryptoError> {
            unreachable!()
        }
        async fn replace_section(
            &self,
            section: String,
            keep: Vec<String>,
        ) -> Result<Vec<String>, CryptoError> {
            if self.replace_fails {
                return Err(CryptoError::NoReply);
            }
            self.replaced.lock().unwrap().push((section, keep));
            Ok(self.withdraw.clone())
        }
        async fn git_credential(&self, _: String, _: String) -> Result<GitCredential, CryptoError> {
            unreachable!()
        }
    }

    fn entries(names: &[&str]) -> Vec<Entry> {
        let body = json!({"credentials": names.iter().map(|n| good_entry(n)).collect::<Vec<_>>()});
        parse(&body).unwrap()
    }

    #[tokio::test]
    async fn each_entry_is_registered_with_its_section_context_and_binding() {
        let crypto = Recorder::default();
        let list = entries(&["github-sei"]);
        let applied = apply(&crypto, "git-repo-observer", 3, &list).await.unwrap();
        assert_eq!(applied.registered, vec!["github-sei".to_string()]);
        let registered = crypto.registered.lock().unwrap();
        let (name, section, secret, ctx, binding) = &registered[0];
        assert_eq!(name, "github-sei");
        assert_eq!(section, "credentials");
        assert_eq!(secret, &STANDARD.decode(&list[0].secret).unwrap());
        assert_eq!(ctx, b"git-repo-observer|credentials|3|github-sei|secret");
        assert_eq!(binding.host_pattern, "github.com");
        assert_eq!(binding.kind, SecretKind::Token);
        assert_eq!(binding.username.as_deref(), Some("oauth2"));
    }

    #[tokio::test]
    async fn an_entry_that_fails_is_reported_and_the_others_still_register() {
        let crypto = Recorder {
            refuse: vec!["bad"],
            ..Default::default()
        };
        let applied = apply(
            &crypto,
            "git-repo-observer",
            1,
            &entries(&["first", "bad", "last"]),
        )
        .await
        .unwrap();
        assert_eq!(
            applied.registered,
            vec!["first".to_string(), "last".to_string()]
        );
        assert_eq!(applied.failed.len(), 1);
        assert_eq!(applied.failed[0].0, "bad");
        assert!(
            applied.failed[0].1.contains("does not open"),
            "the reason was lost: {:?}",
            applied.failed[0].1
        );
    }

    #[tokio::test]
    async fn the_section_is_replaced_keeping_every_name_it_lists_even_one_that_failed() {
        let crypto = Recorder {
            refuse: vec!["bad"],
            withdraw: vec!["old".to_string()],
            ..Default::default()
        };
        let applied = apply(&crypto, "git-repo-observer", 2, &entries(&["good", "bad"]))
            .await
            .unwrap();
        assert_eq!(applied.withdrawn, vec!["old".to_string()]);
        let replaced = crypto.replaced.lock().unwrap();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].0, "credentials");
        assert_eq!(replaced[0].1, vec!["good".to_string(), "bad".to_string()]);
    }

    #[tokio::test]
    async fn an_empty_version_withdraws_everything() {
        let crypto = Recorder {
            withdraw: vec!["old".to_string()],
            ..Default::default()
        };
        let applied = apply(&crypto, "git-repo-observer", 4, &[]).await.unwrap();
        assert!(applied.registered.is_empty());
        assert_eq!(applied.withdrawn, vec!["old".to_string()]);
        assert_eq!(crypto.replaced.lock().unwrap()[0].1, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_section_that_cannot_be_replaced_is_an_error_not_a_success() {
        let crypto = Recorder {
            replace_fails: true,
            ..Default::default()
        };
        assert!(
            apply(&crypto, "git-repo-observer", 1, &entries(&["a"]))
                .await
                .is_err()
        );
    }
}
