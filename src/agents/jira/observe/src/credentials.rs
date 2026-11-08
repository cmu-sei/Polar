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

//! The observer's `credentials` runtime section: the one credential it presents to the Jira site, bound to the hosts
//! it names (docs/architecture/jira.md, "The credential"). The git observer's section is the precedent
//! (`git/observe/src/credentials.rs`, docs/architecture/git-controller.md, "A credential"), and the shape is its shape,
//! so `polar-sign` seals it the same way.
//!
//! The entry carries its secret as ciphertext, an `EscrowedSecret` for this agent bound to
//! `jira-observer|credentials|<version>|<name>|secret`. Here the shape is judged without opening anything, and the
//! entry is handed to the CryptoActor, which opens it in the custody daemon, keeps it with its binding and escrows it.
//! The observer asks it for the credential for the site's host on each request (`CryptoClient::http_credential`): a
//! `token` is sent as `Bearer` (a Data Center or Server personal access token), a `user-password` as Basic, the
//! username the Cloud account's email and the secret its API token. A new token is a new signed version: it reaches
//! the running agent, and the observers start again with it.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use polar_agent_builder::runtime::SectionAuthority;
use polar_agent_builder::{CryptoClient, RuntimeSection};
use polar_crypto::registry::{SecretBinding, SecretKind, pattern_problem};
use polar_escrow::EscrowedSecret;
use serde_json::Value;
use tracing::{info, warn};

/// What the section is called in a document.
pub const SECTION_NAME: &str = "credentials";
/// Most entries one version may carry: the observer reads one site.
pub const MAX_ENTRIES: usize = 1;
/// Longest user name (a Cloud account's email).
pub const MAX_USERNAME: usize = 128;
/// Longest entry name.
pub const MAX_NAME: usize = 64;

pub static CREDENTIALS: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Runtime,
    name: SECTION_NAME,
    doc: "The credential the observer presents to the Jira site, sent only to the hosts it names. Replaced whole by \
          each version: a version with no entry withdraws it. Not needed when JIRA_ANONYMOUS is true.",
    body: r#"{"credentials": {"list_of": {"name": {"type": "text of lowercase letters, digits and hyphens", "required": true, "doc": "what the credential is called"}, "host_pattern": {"type": "host pattern", "required": true, "doc": "the only hosts this credential is ever sent to: the host of JIRA_URL, or *. and a domain"}, "kind": {"type": "token | user-password", "required": true, "doc": "token: a Data Center or Server personal access token, sent as Bearer; user-password: a Jira Cloud API token with the account's email as username, sent as Basic"}, "username": {"type": "text", "required": false, "doc": "required for a user-password: the Jira Cloud account's email"}, "secret": {"type": "ciphertext", "required": true, "secret": true, "doc": "the token itself. In the signed document it is base64 of an EscrowedSecret for this agent, bound to jira-observer|credentials|<version>|<name>|secret; polar-sign makes that from what you type"}}}}"#,
    validate,
};

/// The entry, as the document gives it. The secret is still the base64 of the ciphertext.
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

/// The entry of a body (none: the credential is withdrawn), or every problem with it. Judges the shape and whether
/// the secret is, by its bytes, an escrowed secret; nothing is opened, and no problem says a value.
pub fn parse(body: &Value) -> Result<Option<Entry>, Vec<String>> {
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
            "`credentials` has {} entries, and the observer reads one site, so at most {MAX_ENTRIES} is taken",
            list.len()
        ));
    }
    let mut found = None;
    for (i, item) in list.iter().enumerate() {
        match entry(i, item) {
            Ok(entry) => found = found.or(Some(entry)),
            Err(mut more) => problems.append(&mut more),
        }
    }
    if problems.is_empty() {
        Ok(found)
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
        Some(n)
            if n.is_empty()
                || n.len() > MAX_NAME
                || !n
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') =>
        {
            problems.push(format!(
                "entry {i}: `name` must be 1 to {MAX_NAME} lowercase letters, digits and hyphens"
            ))
        }
        Some(_) => {}
    }
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
            "{label}: a `user-password` credential needs a `username`, the Jira Cloud account's email"
        ));
    }

    let secret = text("secret").unwrap_or_default().to_string();
    match text("secret") {
        None => problems.push(format!("{label}: `secret` must be text")),
        Some(s) => match EscrowedSecret::from_document_text(s) {
            Ok(_) => {}
            Err(polar_escrow::DocumentTextError::NotBase64) => {
                problems.push(format!("{label}: `secret` is not standard base64"))
            }
            Err(polar_escrow::DocumentTextError::Empty) => {
                problems.push(format!("{label}: `secret` is empty"))
            }
            Err(polar_escrow::DocumentTextError::NotASecret) => {
                problems.push(format!("{label}: `secret` is not an escrowed secret"))
            }
        },
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

/// What the entry was bound to when it was encrypted (docs/architecture/signed-configuration.md, "Binding"): the one
/// definition the author's tool uses too.
pub fn context(version: u64, name: &str) -> Result<Vec<u8>, polar_escrow::ContextError> {
    polar_escrow::binding_context(
        crate::BROKER_CLIENT_NAME,
        SECTION_NAME,
        version,
        name,
        "secret",
    )
}

/// Hands a version of the section to the CryptoActor: its entry is registered, then every name it does not list is
/// withdrawn. Returns the name now registered (none: withdrawn), or why the entry could not be (never a value).
pub async fn apply(
    crypto: &dyn CryptoClient,
    version: u64,
    entry: Option<&Entry>,
) -> Result<Option<String>, String> {
    let mut registered = None;
    if let Some(entry) = entry {
        let secret = STANDARD
            .decode(&entry.secret)
            .map_err(|_| format!("the credential `{}` is not base64", entry.name))?;
        let context = context(version, &entry.name)
            .map_err(|e| format!("the credential `{}`: {e}", entry.name))?;
        let binding = SecretBinding {
            host_pattern: entry.host_pattern.clone(),
            kind: entry.kind,
            username: entry.username.clone(),
        };
        crypto
            .register_secret(
                entry.name.clone(),
                SECTION_NAME.to_string(),
                secret,
                context,
                binding,
            )
            .await
            .map_err(|e| {
                warn!(credential = %entry.name, "could not register the credential: {e}");
                format!(
                    "the credential `{}` could not be registered: {e}",
                    entry.name
                )
            })?;
        registered = Some(entry.name.clone());
    }
    let keep = registered.iter().cloned().collect();
    let withdrawn = crypto
        .replace_section(SECTION_NAME.to_string(), keep)
        .await
        .map_err(|e| format!("the credentials section was not applied: {e}"))?;
    info!(
        version,
        registered = registered.is_some(),
        withdrawn = withdrawn.len(),
        "credentials section applied"
    );
    Ok(registered)
}

/// The host a credential is asked for: the host of an `https` JIRA_URL, a name and not an address, with no login of
/// its own. A credential is never sent over a connection that is not encrypted (the git observer's rule,
/// `git_agent_common::remote::https_host`), so a site read with a credential must be https.
pub fn credential_host(jira_url: &str) -> Result<String, String> {
    let url = url::Url::parse(jira_url).map_err(|_| "JIRA_URL cannot be read".to_string())?;
    if url.scheme() != "https" {
        return Err(
            "JIRA_URL is not https, and a credential is sent only over https: set JIRA_URL to the site's https:// \
             address, or JIRA_ANONYMOUS to true to read it without one"
                .to_string(),
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("JIRA_URL carries a login of its own: take it out; the credential is the credentials section".to_string());
    }
    match url.host() {
        Some(url::Host::Domain(domain)) => Ok(domain.to_ascii_lowercase()),
        Some(_) => Err(
            "JIRA_URL names an address, and a credential is bound to a host name: set JIRA_URL to the site's name"
                .to_string(),
        ),
        None => Err("JIRA_URL has no host".to_string()),
    }
}

/// A real escrowed secret, for the tests of this crate.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

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
    pub(crate) fn ciphertext(plaintext: &[u8], context: &[u8]) -> String {
        let escrowed = EscrowedSecret::encrypt_bound(plaintext, public_pem(), context).unwrap();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&escrowed).unwrap();
        STANDARD.encode(bytes.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::ciphertext;
    use super::*;
    use crate::test_crypto::{FakeCrypto, Gives};
    use serde_json::json;

    fn token_entry(name: &str, version: u64) -> Value {
        json!({
            "name": name,
            "host_pattern": "jira.example.org",
            "kind": "token",
            "secret": ciphertext(b"pat-token", &context(version, name).unwrap()),
        })
    }

    #[test]
    fn the_section_is_declared_soundly() {
        assert!(polar_agent_builder::runtime::check_declarations(&[&CREDENTIALS]).is_empty());
    }

    /// A Data Center token, or a Cloud account's email and API token; none withdraws the credential.
    #[test]
    fn a_sound_body_gives_its_one_entry_or_none() {
        let entry = parse(&json!({"credentials": [token_entry("jira", 1)]}))
            .unwrap()
            .unwrap();
        assert_eq!(entry.name, "jira");
        assert_eq!(entry.kind, SecretKind::Token);
        assert_eq!(entry.username, None);

        let cloud = parse(&json!({"credentials": [{
            "name": "acme", "host_pattern": "*.atlassian.net", "kind": "user-password",
            "username": "me@example.org", "secret": ciphertext(b"api-token", b"x")}]}))
        .unwrap()
        .unwrap();
        assert_eq!(cloud.kind, SecretKind::UserPassword);
        assert_eq!(cloud.username.as_deref(), Some("me@example.org"));

        assert_eq!(parse(&json!({"credentials": []})), Ok(None));
    }

    /// Every problem is said, by entry and field, never with a value.
    #[test]
    fn a_body_that_cannot_be_used_is_refused_with_every_problem() {
        let two =
            parse(&json!({"credentials": [token_entry("a", 1), token_entry("b", 1)]})).unwrap_err();
        assert!(two.iter().any(|p| p.contains("at most 1")), "{two:?}");

        let mut no_user = token_entry("jira", 1);
        no_user["kind"] = json!("user-password");
        let problems = parse(&json!({"credentials": [no_user]})).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("needs a `username`")),
            "{problems:?}"
        );

        let problems = parse(&json!({"credentials": [{
            "name": "Jira!", "host_pattern": "*", "kind": "password", "secret": "not-a-secret", "extra": 1}]}))
        .unwrap_err();
        for expected in ["`name`", "`host_pattern`", "`kind`", "`secret`", "`extra`"] {
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{expected}: {problems:?}"
            );
        }
        assert!(
            problems.iter().all(|p| !p.contains("not-a-secret")),
            "{problems:?}"
        );

        let problems = parse(&json!({"other": []})).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("`other`")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`credentials` must be a list")),
            "{problems:?}"
        );
    }

    /// The entry is registered under its name, bound to this agent, the section, the version and the name, with its
    /// host and kind; then the section keeps that name and no other. A version with no entry withdraws it.
    #[tokio::test]
    async fn a_version_is_registered_bound_to_this_agent_and_withdraws_what_it_leaves_out() {
        let crypto = FakeCrypto::giving(Gives::NotRegistered);
        let entry = parse(&json!({"credentials": [token_entry("jira", 7)]}))
            .unwrap()
            .unwrap();
        assert_eq!(
            apply(&crypto, 7, Some(&entry)).await,
            Ok(Some("jira".to_string()))
        );
        {
            let registered = crypto.registered.lock().unwrap();
            let [(name, section, binding, context)] = &registered[..] else {
                panic!("one registered: {registered:?}");
            };
            assert_eq!(name, "jira");
            assert_eq!(section, SECTION_NAME);
            assert_eq!(binding.host_pattern, "jira.example.org");
            assert_eq!(binding.kind, SecretKind::Token);
            assert_eq!(
                context.as_slice(),
                b"jira-observer|credentials|7|jira|secret"
            );
        }
        assert_eq!(apply(&crypto, 8, None).await, Ok(None));
        assert_eq!(
            *crypto.replaced.lock().unwrap(),
            [
                (SECTION_NAME.to_string(), vec!["jira".to_string()]),
                (SECTION_NAME.to_string(), vec![])
            ]
        );
    }

    /// A credential goes only over https, to a host name, as git's do.
    #[test]
    fn the_credential_is_asked_for_the_host_of_an_https_address_only() {
        assert_eq!(
            credential_host("https://Jira.Example.org/jira").as_deref(),
            Ok("jira.example.org")
        );
        assert_eq!(
            credential_host("https://jira.example.org:8443").as_deref(),
            Ok("jira.example.org")
        );
        for (refused, why) in [
            ("http://jira.example.org", "https"),
            ("https://10.0.0.5", "address"),
            ("https://[::1]", "address"),
            ("https://me:pw@jira.example.org", "login"),
        ] {
            let error = credential_host(refused).unwrap_err();
            assert!(error.contains(why), "{refused}: {error}");
        }
    }
}
