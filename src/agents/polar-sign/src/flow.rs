//! `polar-sign sign`, end to end but for the terminal and the network: read the author's file, check it against
//! the agent's scaffold, seal its secrets to the agent's escrow key, and sign. The agent's certificate comes
//! from a function the caller passes (the HTTP request in the program, a stand-in in a test), and is asked for
//! only if there is a plaintext secret to seal.

use crate::author::{prepare, read_input};
use crate::fetch::{escrow_identity, escrow_key};
use crate::scaffold::Scaffold;
use crate::seal::{ReadSecret, Sealed};
use crate::{Identity, sign_to_dhall};
use polar_escrow::EscrowedSecret;

/// How to sign.
pub struct Signing<'a> {
    /// The author's certificate chain and key.
    pub identity: &'a Identity,
    /// The CA certificates the author trusts (`ca.pem`).
    pub trust_pem: &'a [String],
    /// Seconds since the epoch.
    pub now: i64,
    /// The identity the agent's escrow certificate must carry, if not the usual one.
    pub escrow_identity: Option<&'a str>,
}

/// A signed document and what it is.
#[derive(Debug)]
pub struct Signed {
    /// The Dhall file to commit.
    pub dhall: String,
    pub agent: String,
    /// The section, or `None` for a document of settings.
    pub section: Option<String>,
    pub version: u64,
    pub id: String,
    pub sealed: Sealed,
}

/// Signs what the author wrote in `text`. Every problem comes back together and nothing is signed if there is one.
pub fn sign_file(
    text: &str,
    scaffold: &Scaffold,
    how: &Signing<'_>,
    fetch_certificate: &dyn Fn(&str) -> Result<String, String>,
    read_secret: &mut ReadSecret<'_>,
) -> Result<Signed, Vec<String>> {
    let input = read_input(text).map_err(|e| vec![e])?;
    let agent = input.agent.clone();
    let version = input.version;
    let identity = how
        .escrow_identity
        .map(str::to_string)
        .unwrap_or_else(|| escrow_identity(&agent));
    let trust = polar_signing::Trust::from_pem(how.trust_pem)
        .map_err(|e| vec![format!("the CA certificate cannot be used: {e}")])?;

    // The key is fetched, and checked, the first time a secret is sealed and not before.
    let mut key: Option<String> = None;
    let mut encrypt = |plaintext: &[u8], context: &[u8]| -> Result<String, String> {
        if key.is_none() {
            let certificate = fetch_certificate(&identity)?;
            key = Some(escrow_key(&certificate, &trust, how.now, &identity)?);
        }
        let key_pem = key.as_deref().expect("set above");
        EscrowedSecret::encrypt_bound(plaintext, key_pem, context)
            .map_err(|e| format!("cannot encrypt: {e}"))?
            .to_document_text()
    };

    let prepared = prepare(input, scaffold, &mut encrypt, read_secret)?;
    let id = prepared.payload["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let dhall = sign_to_dhall(prepared.payload, how.identity).map_err(|e| vec![e])?;
    Ok(Signed {
        dhall,
        agent,
        section: prepared.section,
        version,
        id,
        sealed: prepared.sealed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{split_pem, verify_source};
    use polar_signing::testing::{EscrowSpec, Spec, escrow_certificate_for, now, root, signer};
    use rsa::pkcs8::EncodePublicKey;
    use serde_json::json;
    use std::cell::RefCell;

    fn scaffold() -> Scaffold {
        Scaffold::parse(
            &json!({
                "agent": {"name": "git-repo-observer"},
                "sections": [{"name": "b", "variables": [
                    {"name": "RUST_LOG", "type": "text", "mutability": "dynamic", "default": "info"}]}],
                "runtime": [{"name": "credentials", "doc": "d", "body": {
                    "credentials": {"list_of": {
                        "name": {"type": "text", "required": true},
                        "host_pattern": {"type": "host pattern", "required": true},
                        "kind": {"type": "token | user-password", "required": true},
                        "secret": {"type": "ciphertext", "required": true, "secret": true}
                    }}
                }}]
            })
            .to_string(),
        )
        .unwrap()
    }

    struct World {
        ca: polar_signing::testing::Authority,
        author: Identity,
        private: rsa::RsaPrivateKey,
        escrow_cert: String,
    }

    fn world() -> World {
        let ca = root("Polar Internal CA");
        let s = signer(
            &ca,
            Spec {
                capabilities: vec!["c:update-runtime"],
                ..Spec::default()
            },
        );
        let mut rng = rand::rng();
        let private = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_pem = private
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let escrow_cert = escrow_certificate_for(&ca, EscrowSpec::default(), &public_pem);
        World {
            author: Identity {
                chain: vec![s.cert_pem],
                key_pem: s.key_pem,
            },
            ca,
            private,
            escrow_cert,
        }
    }

    fn file(secret: &str) -> String {
        json!({
            "agent": "git-repo-observer", "section": "credentials", "version": 5, "id": "cred-0005",
            "body": {"credentials": [
                {"name": "github-sei", "host_pattern": "github.com", "kind": "token", "secret": secret}
            ]}
        })
        .to_string()
    }

    fn no_stdin() -> impl FnMut() -> Result<String, String> {
        || Err("no standard input".to_string())
    }

    #[test]
    fn a_credential_is_sealed_to_the_agent_signed_and_opens_only_for_that_entry() {
        let w = world();
        let how = Signing {
            identity: &w.author,
            trust_pem: std::slice::from_ref(&w.ca.pem),
            now: now(),
            escrow_identity: None,
        };
        let asked = RefCell::new(Vec::new());
        let fetch = |identity: &str| {
            asked.borrow_mut().push(identity.to_string());
            Ok(w.escrow_cert.clone())
        };
        let signed = sign_file(
            &file("ghp-the-token"),
            &scaffold(),
            &how,
            &fetch,
            &mut no_stdin(),
        )
        .unwrap();

        assert_eq!(
            *asked.borrow(),
            vec!["git-repo-observer.polar.serviceaccount.cluster.local".to_string()],
            "one request, for the agent's identity"
        );
        assert_eq!(
            (
                signed.agent.as_str(),
                signed.section.as_deref(),
                signed.version
            ),
            ("git-repo-observer", Some("credentials"), 5)
        );
        assert_eq!(signed.id, "cred-0005");
        assert_eq!(signed.sealed.encrypted, 1);
        assert!(
            !signed.dhall.contains("ghp-the-token"),
            "the secret is nowhere in the file"
        );

        // the file verifies as the author's, for the right agent and section
        let (who, payload) = verify_source(&signed.dhall, &split_pem(&w.ca.pem), now()).unwrap();
        assert_eq!(who.username.as_deref(), Some("alice"));
        assert_eq!(payload["type"], "runtime");
        assert_eq!(payload["target"]["agent"], "git-repo-observer");
        assert_eq!(payload["section"], "credentials");

        // and the ciphertext opens, with the agent's private key, under this entry's context and no other
        let text = payload["body"]["credentials"][0]["secret"]
            .as_str()
            .unwrap();
        let sealed = EscrowedSecret::from_document_text(text).unwrap();
        let context = polar_escrow::binding_context(
            "git-repo-observer",
            "credentials",
            5,
            "github-sei",
            "secret",
        )
        .unwrap();
        assert_eq!(
            sealed
                .decrypt_bound(&w.private, &context)
                .unwrap()
                .as_slice(),
            b"ghp-the-token"
        );
        for other in [
            polar_escrow::binding_context(
                "git-repo-observer",
                "credentials",
                6,
                "github-sei",
                "secret",
            )
            .unwrap(),
            polar_escrow::binding_context(
                "git-repo-observer",
                "credentials",
                5,
                "another",
                "secret",
            )
            .unwrap(),
        ] {
            assert!(sealed.decrypt_bound(&w.private, &other).is_err());
        }
    }

    #[test]
    fn the_certificate_is_asked_for_once_however_many_secrets_there_are() {
        let w = world();
        let how = Signing {
            identity: &w.author,
            trust_pem: std::slice::from_ref(&w.ca.pem),
            now: now(),
            escrow_identity: None,
        };
        let asked = RefCell::new(0);
        let fetch = |_: &str| {
            *asked.borrow_mut() += 1;
            Ok(w.escrow_cert.clone())
        };
        let text = json!({
            "agent": "git-repo-observer", "section": "credentials", "version": 1,
            "body": {"credentials": [
                {"name": "a", "host_pattern": "github.com", "kind": "token", "secret": "one"},
                {"name": "b", "host_pattern": "gitlab.com", "kind": "token", "secret": "two"}
            ]}
        })
        .to_string();
        let signed = sign_file(&text, &scaffold(), &how, &fetch, &mut no_stdin()).unwrap();
        assert_eq!(signed.sealed.encrypted, 2);
        assert_eq!(*asked.borrow(), 1);
    }

    #[test]
    fn a_document_with_no_secret_needs_no_certificate_and_no_network() {
        let w = world();
        let how = Signing {
            identity: &w.author,
            trust_pem: std::slice::from_ref(&w.ca.pem),
            now: now(),
            escrow_identity: None,
        };
        let fetch = |_: &str| -> Result<String, String> { panic!("the gateway was asked") };
        let settings =
            json!({"agent": "git-repo-observer", "version": 2, "settings": {"RUST_LOG": "debug"}})
                .to_string();
        let signed = sign_file(&settings, &scaffold(), &how, &fetch, &mut no_stdin()).unwrap();
        assert_eq!(signed.section, None);
        let (_, payload) = verify_source(&signed.dhall, &split_pem(&w.ca.pem), now()).unwrap();
        assert_eq!(payload["type"], "configuration");
        assert_eq!(payload["changes"][0]["value"], "debug");

        let empty = json!({"agent": "git-repo-observer", "section": "credentials", "version": 1,
            "body": {"credentials": []}})
        .to_string();
        assert!(sign_file(&empty, &scaffold(), &how, &fetch, &mut no_stdin()).is_ok());
    }

    #[test]
    fn a_certificate_that_cannot_be_believed_means_nothing_is_signed() {
        let w = world();
        let how = Signing {
            identity: &w.author,
            trust_pem: std::slice::from_ref(&w.ca.pem),
            now: now(),
            escrow_identity: None,
        };
        // the substitution: a certificate from a CA of the attacker's own
        let attacker = root("Polar Internal CA");
        let forged = escrow_certificate_for(
            &attacker,
            EscrowSpec::default(),
            polar_signing::testing::ESCROW_PUBLIC_KEY_PEM,
        );
        let fetch = |_: &str| Ok(forged.clone());
        let problems = sign_file(
            &file("ghp-secret"),
            &scaffold(),
            &how,
            &fetch,
            &mut no_stdin(),
        )
        .unwrap_err();
        assert!(problems[0].contains("cannot be believed"), "{problems:?}");
        assert!(problems.iter().all(|p| !p.contains("ghp-secret")));
    }

    #[test]
    fn a_gateway_that_does_not_answer_is_reported_by_field() {
        let w = world();
        let how = Signing {
            identity: &w.author,
            trust_pem: std::slice::from_ref(&w.ca.pem),
            now: now(),
            escrow_identity: None,
        };
        let fetch = |_: &str| Err::<String, String>("cannot reach the gateway".to_string());
        let problems = sign_file(
            &file("ghp-secret"),
            &scaffold(),
            &how,
            &fetch,
            &mut no_stdin(),
        )
        .unwrap_err();
        assert_eq!(
            problems,
            vec!["`credentials[0].secret`: cannot reach the gateway".to_string()]
        );
    }

    #[test]
    fn the_identity_asked_for_can_be_overridden() {
        let w = world();
        let id = "git-repo-observer.other.serviceaccount.cluster.local";
        let how = Signing {
            identity: &w.author,
            trust_pem: std::slice::from_ref(&w.ca.pem),
            now: now(),
            escrow_identity: Some(id),
        };
        let cert = escrow_certificate_for(
            &w.ca,
            EscrowSpec {
                identity: "git-repo-observer.other.serviceaccount.cluster.local",
                ..EscrowSpec::default()
            },
            polar_signing::testing::ESCROW_PUBLIC_KEY_PEM,
        );
        let asked = RefCell::new(String::new());
        let fetch = |i: &str| {
            *asked.borrow_mut() = i.to_string();
            Ok(cert.clone())
        };
        sign_file(&file("tok"), &scaffold(), &how, &fetch, &mut no_stdin()).unwrap();
        assert_eq!(*asked.borrow(), id);
    }

    #[test]
    fn nothing_is_signed_when_the_file_is_wrong_and_the_gateway_is_never_asked() {
        let w = world();
        let how = Signing {
            identity: &w.author,
            trust_pem: std::slice::from_ref(&w.ca.pem),
            now: now(),
            escrow_identity: None,
        };
        let fetch = |_: &str| -> Result<String, String> { panic!("the gateway was asked") };
        let wrong = json!({"agent": "git-repo-observer", "section": "credentials", "version": 1,
            "body": {"credentials": [{"name": "a", "secret": "tok"}]}})
        .to_string();
        let problems = sign_file(&wrong, &scaffold(), &how, &fetch, &mut no_stdin()).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`credentials[0].kind` is required")),
            "{problems:?}"
        );
        assert!(sign_file("not json", &scaffold(), &how, &fetch, &mut no_stdin()).is_err());
    }

    #[test]
    fn a_ca_certificate_that_is_not_one_is_reported() {
        let w = world();
        let bad = vec!["not a certificate".to_string()];
        let how = Signing {
            identity: &w.author,
            trust_pem: &bad,
            now: now(),
            escrow_identity: None,
        };
        let fetch = |_: &str| -> Result<String, String> { panic!("asked") };
        let problems =
            sign_file(&file("x"), &scaffold(), &how, &fetch, &mut no_stdin()).unwrap_err();
        assert!(
            problems[0].contains("CA certificate cannot be used"),
            "{problems:?}"
        );
    }
}
