//! What the CryptoActor keeps for a secret that reached the agent, and the rules that decide where it may go
//! (docs/architecture/credentials-agent-architecture.md, "Named operations on registered secrets").
//!
//! This file is the pure part: the binding, what a host pattern matches, and the form in which a secret and its
//! binding are escrowed together. The actor that holds and uses them is in `lib.rs`.

use std::net::IpAddr;

/// The longest a host name can be, and the longest one label can be (RFC 1035).
const MAX_HOST: usize = 253;
const MAX_LABEL: usize = 63;

/// Whether `name` is a bare DNS name made of lowercase letters, digits, hyphens and dots: labels that are not
/// empty, not too long, and neither start nor end with a hyphen. Not an address, and ASCII only (an
/// internationalised name is its punycode form, or it does not match: this fails closed).
fn is_bare_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_HOST || name.parse::<IpAddr>().is_ok() {
        return false;
    }
    // Written with leading digits and dots only (127.1, 0x7f.0.0.1) a name is still read as an address by
    // some resolvers, so a name whose last label is all digits or looks like a number is refused.
    let last = name.rsplit('.').next().unwrap_or("");
    if last.chars().all(|c| c.is_ascii_digit()) || last.starts_with("0x") {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= MAX_LABEL
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// What is wrong with a host pattern, if anything. A pattern is a bare name (`github.com`) or `*.` and a domain
/// (`*.example.org`), compared in lowercase. Anything that could match more than it says (a stray `*`, a port,
/// userinfo, a trailing dot, an address) is refused.
pub fn pattern_problem(pattern: &str) -> Option<String> {
    let domain = pattern.strip_prefix("*.").unwrap_or(pattern);
    if domain.contains('*') {
        return Some("a host pattern may have a `*` only as a leading `*.`".to_string());
    }
    if !is_bare_name(&domain.to_ascii_lowercase()) || !domain.is_ascii() {
        return Some(
            "a host pattern is a bare host name or `*.` and a domain, with no port, userinfo, trailing dot, \
             address or non-ASCII character"
                .to_string(),
        );
    }
    None
}

/// Whether `host` is one `pattern` allows a secret to go to. `*.example.org` matches that domain with one or
/// more labels before it, at any depth, and not the bare domain; a dot is required, so a lookalike
/// (`evilexample.org`) does not match. Case is ignored. `host` must itself be a bare name, so a port, userinfo,
/// trailing dot or address never matches, and a pattern that is not valid (`pattern_problem`) matches nothing.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    if pattern_problem(pattern).is_some() || !host.is_ascii() {
        return false;
    }
    let host = host.to_ascii_lowercase();
    if !is_bare_name(&host) {
        return false;
    }
    let pattern = pattern.to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(domain) => host
            .strip_suffix(domain)
            .is_some_and(|before| before.ends_with('.')),
        None => host == pattern,
    }
}

/// What kind of credential a secret is: how it is presented to whatever it authenticates to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretKind {
    /// A bearer token. Git is given a username (`oauth2` unless the binding names one) and the token.
    Token,
    /// A user name and a password. The binding holds the user name.
    UserPassword,
}

impl SecretKind {
    /// The `kind` label a secret of this kind is stored with, which policy can speak of (`resource.kind`). The
    /// same text the kind is written with in the escrowed record.
    pub fn as_label(&self) -> &'static str {
        match self {
            SecretKind::Token => "token",
            SecretKind::UserPassword => "user-password",
        }
    }
}

/// Where a secret may be used, and what it is. Escrowed with the secret, so a restart brings it back with it
/// and the host check cannot be lost.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretBinding {
    /// The only hosts the secret is ever given for (`host_matches`).
    pub host_pattern: String,
    pub kind: SecretKind,
    pub username: Option<String>,
}

impl SecretBinding {
    /// Whether the binding can be used at all: its pattern must be valid.
    pub fn check(&self) -> Result<(), RecordError> {
        match pattern_problem(&self.host_pattern) {
            Some(problem) => Err(RecordError::BadBinding(problem)),
            None => Ok(()),
        }
    }
}

/// What git is given for a host: a user name and a secret, wiped when dropped.
pub struct GitCredential {
    pub username: String,
    pub secret: zeroize::Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for GitCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCredential")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Why a secret was not given. None of them says anything about the secret itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UseRefusal {
    #[error("the secret is not bound to that host")]
    HostNotBound,
    #[error("the binding names no user name, and a user name and password need one")]
    NoUsername,
    #[error("the secret is not text, so it cannot be given to git")]
    NotText,
}

/// The credential git is given for `host`, or why not. The host is checked first, so a caller who may not have
/// the secret learns nothing else about it.
pub fn git_credential(
    binding: &SecretBinding,
    secret: &[u8],
    host: &str,
) -> Result<GitCredential, UseRefusal> {
    if !host_matches(&binding.host_pattern, host) {
        return Err(UseRefusal::HostNotBound);
    }
    if std::str::from_utf8(secret).is_err() {
        return Err(UseRefusal::NotText);
    }
    let username = match (binding.kind, &binding.username) {
        (_, Some(name)) => name.clone(),
        (SecretKind::Token, None) => "oauth2".to_string(),
        (SecretKind::UserPassword, None) => return Err(UseRefusal::NoUsername),
    };
    Ok(GitCredential {
        username,
        secret: zeroize::Zeroizing::new(secret.to_vec()),
    })
}

/// Why an escrowed record could not be made or read. The messages are fixed: a parser's own message can repeat a
/// value, and a value here may be a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    #[error("the escrowed record is not a credential of the expected form")]
    Malformed,
    #[error("the secret is not text, which the escrowed form needs")]
    NotText,
    #[error("the binding is not usable: {0}")]
    BadBinding(String),
}

#[derive(serde::Serialize)]
struct RecordOut<'a> {
    host_pattern: &'a str,
    kind: SecretKind,
    username: Option<&'a str>,
    secret: &'a str,
}

#[derive(serde::Deserialize)]
struct RecordIn {
    host_pattern: String,
    kind: SecretKind,
    username: Option<String>,
    secret: String,
}

/// The bytes escrowed for a secret and its binding together, as JSON, so a field added later does not break an
/// older record. Wiped when dropped.
pub fn escrow_form(
    binding: &SecretBinding,
    secret: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, RecordError> {
    binding.check()?;
    let secret = std::str::from_utf8(secret).map_err(|_| RecordError::NotText)?;
    serde_json::to_vec(&RecordOut {
        host_pattern: &binding.host_pattern,
        kind: binding.kind,
        username: binding.username.as_deref(),
        secret,
    })
    .map(zeroize::Zeroizing::new)
    .map_err(|_| RecordError::Malformed)
}

/// The binding and the secret from what `escrow_form` made. A record that is not of that form, or whose binding
/// is not usable, is refused and not trusted.
pub fn from_escrow_form(
    bytes: &[u8],
) -> Result<(SecretBinding, zeroize::Zeroizing<Vec<u8>>), RecordError> {
    let record: RecordIn = serde_json::from_slice(bytes).map_err(|_| RecordError::Malformed)?;
    let binding = SecretBinding {
        host_pattern: record.host_pattern,
        kind: record.kind,
        username: record.username,
    };
    binding.check()?;
    Ok((binding, zeroize::Zeroizing::new(record.secret.into_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(pattern: &str, kind: SecretKind, username: Option<&str>) -> SecretBinding {
        SecretBinding {
            host_pattern: pattern.to_string(),
            kind,
            username: username.map(str::to_string),
        }
    }

    #[test]
    fn a_token_is_given_to_git_with_the_conventional_username_unless_the_binding_names_one() {
        let plain = binding("github.com", SecretKind::Token, None);
        let got = git_credential(&plain, b"ghp-token", "github.com").unwrap();
        assert_eq!(got.username, "oauth2");
        assert_eq!(got.secret.as_slice(), b"ghp-token");

        let named = binding("github.com", SecretKind::Token, Some("daveman1010221"));
        let got = git_credential(&named, b"ghp-token", "github.com").unwrap();
        assert_eq!(got.username, "daveman1010221");
    }

    #[test]
    fn a_user_and_password_need_the_username_the_binding_holds() {
        let ok = binding("git.example.org", SecretKind::UserPassword, Some("alice"));
        let got = git_credential(&ok, b"hunter2", "git.example.org").unwrap();
        assert_eq!(
            (got.username.as_str(), got.secret.as_slice()),
            ("alice", &b"hunter2"[..])
        );

        let no_user = binding("git.example.org", SecretKind::UserPassword, None);
        assert_eq!(
            git_credential(&no_user, b"hunter2", "git.example.org").unwrap_err(),
            UseRefusal::NoUsername
        );
    }

    #[test]
    fn a_secret_is_never_given_for_a_host_the_binding_does_not_name() {
        let b = binding("github.com", SecretKind::Token, None);
        for host in [
            "evil.example.org",
            "github.com.evil.org",
            "gist.github.com",
            "github.com:443",
            "",
        ] {
            assert_eq!(
                git_credential(&b, b"ghp-token", host).unwrap_err(),
                UseRefusal::HostNotBound,
                "{host:?}"
            );
        }
    }

    #[test]
    fn the_host_is_checked_before_anything_about_the_secret_is_looked_at() {
        // A secret that is not text and a user-password binding with no username are both wrong, but a wrong host
        // is the answer: nothing else about a secret is said to a caller who may not have it.
        let b = binding("github.com", SecretKind::UserPassword, None);
        assert_eq!(
            git_credential(&b, &[0xff, 0xfe], "evil.example.org").unwrap_err(),
            UseRefusal::HostNotBound
        );
    }

    #[test]
    fn a_secret_that_is_not_text_cannot_be_given_to_git() {
        let b = binding("github.com", SecretKind::Token, None);
        assert_eq!(
            git_credential(&b, &[0xff, 0xfe, 0x00], "github.com").unwrap_err(),
            UseRefusal::NotText
        );
    }

    #[test]
    fn a_binding_with_an_invalid_pattern_gives_nothing() {
        let b = binding("*", SecretKind::Token, None);
        assert_eq!(
            git_credential(&b, b"ghp-token", "github.com").unwrap_err(),
            UseRefusal::HostNotBound
        );
    }

    #[test]
    fn a_secret_and_its_binding_are_escrowed_together_and_come_back_whole() {
        let b = binding("*.example.org", SecretKind::UserPassword, Some("alice"));
        let bytes = escrow_form(&b, b"hunter2").unwrap();
        let (back, secret) = from_escrow_form(&bytes).unwrap();
        assert_eq!(back, b);
        assert_eq!(secret.as_slice(), b"hunter2");
    }

    #[test]
    fn the_escrowed_form_is_json_with_the_fields_the_design_names() {
        let b = binding("github.com", SecretKind::Token, Some("oauth2"));
        let bytes = escrow_form(&b, b"ghp-token").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["host_pattern"], "github.com");
        assert_eq!(v["kind"], "token");
        assert_eq!(v["username"], "oauth2");
        assert_eq!(v["secret"], "ghp-token");
        let up = escrow_form(
            &binding("h.example.org", SecretKind::UserPassword, None),
            b"x",
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&up).unwrap();
        assert_eq!(v["kind"], "user-password");
        assert!(v["username"].is_null());
    }

    #[test]
    fn a_secret_that_is_not_text_is_not_escrowed_in_a_form_that_cannot_hold_it() {
        let b = binding("github.com", SecretKind::Token, None);
        assert!(escrow_form(&b, &[0xff, 0xfe]).is_err());
    }

    #[test]
    fn an_escrowed_record_that_is_garbage_or_has_a_bad_binding_is_refused_not_trusted() {
        assert!(from_escrow_form(b"not json").is_err());
        assert!(from_escrow_form(br#"{"host_pattern":"github.com"}"#).is_err());
        assert!(
            from_escrow_form(
                br#"{"host_pattern":"*","kind":"token","username":null,"secret":"x"}"#
            )
            .is_err()
        );
        assert!(
            from_escrow_form(
                br#"{"host_pattern":"github.com","kind":"nonsense","username":null,"secret":"x"}"#
            )
            .is_err()
        );
        // A field added later is ignored, so an older reader still reads a newer record.
        assert!(from_escrow_form(br#"{"host_pattern":"github.com","kind":"token","username":null,"secret":"x","later":1}"#).is_ok());
    }

    #[test]
    fn nothing_prints_the_secret() {
        let b = binding("github.com", SecretKind::Token, None);
        let cred = git_credential(&b, b"super-secret-token", "github.com").unwrap();
        assert!(!format!("{cred:?}").contains("super-secret-token"));
        let err = from_escrow_form(
            br#"{"host_pattern":"*","kind":"token","username":null,"secret":"super-secret-token"}"#,
        )
        .unwrap_err();
        assert!(!format!("{err} {err:?}").contains("super-secret-token"));
    }

    #[test]
    fn a_kinds_label_is_the_name_it_is_written_with() {
        for kind in [SecretKind::Token, SecretKind::UserPassword] {
            let written = serde_json::to_value(kind).unwrap();
            assert_eq!(
                written,
                serde_json::Value::String(kind.as_label().to_string())
            );
        }
        assert_eq!(SecretKind::Token.as_label(), "token");
        assert_eq!(SecretKind::UserPassword.as_label(), "user-password");
    }

    #[test]
    fn a_bare_host_pattern_matches_that_host_exactly_and_ignores_case() {
        assert!(host_matches("github.com", "github.com"));
        assert!(host_matches("GitHub.com", "github.COM"));
        assert!(!host_matches("github.com", "gist.github.com"));
        assert!(!host_matches("github.com", "evilgithub.com"));
        assert!(!host_matches("github.com", "github.com.evil.org"));
    }

    #[test]
    fn a_wildcard_matches_the_domain_at_any_depth_and_not_the_bare_domain() {
        assert!(host_matches("*.example.org", "api.example.org"));
        assert!(host_matches("*.example.org", "a.b.example.org"));
        assert!(host_matches("*.Example.ORG", "API.example.org"));
        assert!(!host_matches("*.example.org", "example.org"));
    }

    #[test]
    fn a_wildcard_needs_the_dot_so_a_lookalike_does_not_match() {
        assert!(!host_matches("*.example.org", "evilexample.org"));
        assert!(!host_matches("*.example.org", "example.org.evil.com"));
        assert!(!host_matches("*.example.org", "xexample.org"));
    }

    #[test]
    fn a_host_must_be_a_bare_name() {
        for host in [
            "github.com:443",
            "user@github.com",
            "user:secret@github.com",
            "github.com/path",
            "github.com.",
            ".github.com",
            "git hub.com",
            "github.com\n",
            "",
            "a..b.example.org",
            "-a.example.org",
            "a-.example.org",
        ] {
            assert!(
                !host_matches("github.com", host),
                "{host:?} matched github.com"
            );
            assert!(
                !host_matches("*.example.org", host),
                "{host:?} matched *.example.org"
            );
        }
    }

    #[test]
    fn an_address_never_matches_even_a_pattern_written_as_that_address() {
        for host in [
            "140.82.112.3",
            "::1",
            "[::1]",
            "2001:db8::1",
            "0x7f.0.0.1",
            "127.1",
        ] {
            assert!(!host_matches(host, host), "{host} matched itself");
            assert!(!host_matches("*.example.org", host));
        }
    }

    #[test]
    fn a_host_that_is_not_ascii_never_matches() {
        assert!(!host_matches("github.com", "gıthub.com"));
        assert!(!host_matches("*.example.org", "münchen.example.org"));
        assert!(!host_matches("münchen.de", "münchen.de"));
    }

    #[test]
    fn a_host_longer_than_a_name_can_be_never_matches() {
        let label = "a".repeat(64);
        assert!(!host_matches(
            "*.example.org",
            &format!("{label}.example.org")
        ));
        let long = format!("{}.example.org", ["a"; 130].join("."));
        assert!(!host_matches("*.example.org", &long));
    }

    #[test]
    fn a_valid_pattern_is_a_bare_name_or_star_dot_and_a_domain() {
        for pattern in [
            "github.com",
            "*.example.org",
            "localhost",
            "a-b.c-d.example.org",
        ] {
            assert_eq!(pattern_problem(pattern), None, "{pattern}");
        }
    }

    #[test]
    fn a_pattern_that_could_match_more_than_it_says_is_refused() {
        for pattern in [
            "",
            "*",
            "*.",
            "**.example.org",
            "*.*.example.org",
            "a.*.org",
            "*example.org",
            "example.*",
            "github.com:443",
            "user@github.com",
            "github.com/",
            "github.com.",
            " github.com",
            "140.82.112.3",
            "::1",
            "münchen.de",
        ] {
            assert!(
                pattern_problem(pattern).is_some(),
                "{pattern:?} was accepted"
            );
        }
    }

    #[test]
    fn an_invalid_pattern_matches_nothing() {
        for pattern in ["", "*", "**.example.org", "a.*.org", "example.*"] {
            assert!(!host_matches(pattern, "example.org"), "{pattern:?}");
            assert!(!host_matches(pattern, "a.example.org"), "{pattern:?}");
        }
    }
}
