//! Registry credentials, looked up in a Docker credential file.
//!
//! The lookup is the `docker_credential` crate's, which reads `DOCKER_CONFIG` from the process
//! environment. The resolver's configuration is a document, so the directory arrives as a value
//! and the file is read from it here.

use docker_credential::{CredentialRetrievalError, DockerCredential};
use oci_client::secrets::RegistryAuth;
use std::path::Path;
use tracing::{debug, error, warn};

/// The keys under which a registry's credentials may be stored, most specific first. No `http://`
/// candidate: registry credential lookups must not silently fall back to a cleartext transport.
/// If an HTTP-only registry is ever needed, gate it behind explicit configuration rather than
/// adding it back unconditionally. See #218.
pub fn candidates(base: &str) -> Vec<String> {
    vec![
        base.to_string(),
        format!("https://{}", base),
        format!("https://{}/", base),
        format!("{}/v1/", base),
        format!("https://{}/v1/", base),
    ]
}

/// `registry` without a scheme or a trailing slash.
pub fn normalize_registry_host(s: &str) -> String {
    s.trim()
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

/// One lookup: in the file `<docker_config>/config.json` when a directory is given, otherwise
/// where the crate looks (`DOCKER_CONFIG`, then the home directory's `.docker`).
fn lookup(
    docker_config: Option<&Path>,
    key: &str,
) -> Result<DockerCredential, CredentialRetrievalError> {
    match docker_config {
        None => docker_credential::get_credential(key),
        Some(dir) => {
            let file = std::fs::File::open(dir.join("config.json"))
                .map_err(|_| CredentialRetrievalError::ConfigNotFound)?;
            docker_credential::get_credential_from_reader(std::io::BufReader::new(file), key)
        }
    }
}

/// The credentials for `registry`, or anonymous access when there are none to be had.
pub fn registry_auth(
    docker_config: Option<&Path>,
    registry: &str,
) -> Result<RegistryAuth, CredentialRetrievalError> {
    debug!("Resolving OCI credentials for registry: {registry}");
    let base = normalize_registry_host(registry);
    for candidate in candidates(&base) {
        debug!("Attempting docker-credential lookup for key: {candidate}");
        match lookup(docker_config, &candidate) {
            Ok(DockerCredential::UsernamePassword(u, p)) => {
                debug!("Resolved credentials from docker config for key {candidate}");
                return Ok(RegistryAuth::Basic(u, p));
            }
            Ok(DockerCredential::IdentityToken(_)) => {
                // An identity token is not usable for manifest pulls; try the next candidate.
                warn!("IdentityToken returned for {candidate} — not supported. Skipping.");
                continue;
            }
            Err(CredentialRetrievalError::ConfigNotFound) => {
                debug!("No docker config found — falling back to anonymous authentication");
                return Ok(RegistryAuth::Anonymous);
            }
            Err(CredentialRetrievalError::NoCredentialConfigured) => {
                debug!(
                    "No credentials configured for {candidate} — falling back to anonymous \
                     authentication"
                );
                return Ok(RegistryAuth::Anonymous);
            }
            Err(e) => {
                error!("Error reading credentials for {candidate}: {e}");
                return Err(e);
            }
        }
    }
    warn!("No usable credentials found — falling back to anonymous for {base}");
    Ok(RegistryAuth::Anonymous)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(config_json: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), config_json).unwrap();
        dir
    }

    /// base64 of "user:pass"
    const USER_PASS: &str = "dXNlcjpwYXNz";

    #[test]
    fn host_variants_are_normalised() {
        for (input, expected) in [
            ("https://example.com/", "example.com"),
            ("http://example.com", "example.com"),
            ("example.com/", "example.com"),
            (" example.com ", "example.com"),
        ] {
            assert_eq!(normalize_registry_host(input), expected);
        }
    }

    #[test]
    fn candidates_never_include_a_cleartext_scheme() {
        let c = candidates("example.com");
        assert!(c.contains(&"example.com".to_string()));
        assert!(c.contains(&"https://example.com".to_string()));
        assert!(!c.iter().any(|c| c.starts_with("http://")));
    }

    #[test]
    fn a_stored_username_and_password_are_found_in_the_given_directory() {
        let dir = dir_with(&format!(
            r#"{{"auths":{{"registry.example.com":{{"auth":"{USER_PASS}"}}}}}}"#
        ));
        match registry_auth(Some(dir.path()), "registry.example.com").unwrap() {
            RegistryAuth::Basic(user, pass) => {
                assert_eq!((user.as_str(), pass.as_str()), ("user", "pass"))
            }
            other => panic!("expected basic credentials, got {other:?}"),
        }
    }

    #[test]
    fn a_scheme_on_the_registry_is_not_in_the_way() {
        let dir = dir_with(&format!(
            r#"{{"auths":{{"registry.example.com":{{"auth":"{USER_PASS}"}}}}}}"#
        ));
        assert!(matches!(
            registry_auth(Some(dir.path()), "https://registry.example.com/").unwrap(),
            RegistryAuth::Basic(..)
        ));
    }

    #[test]
    fn a_registry_with_no_entry_is_asked_anonymously() {
        let dir = dir_with(r#"{"auths":{}}"#);
        assert!(matches!(
            registry_auth(Some(dir.path()), "registry.example.com").unwrap(),
            RegistryAuth::Anonymous
        ));
    }

    #[test]
    fn an_empty_config_is_anonymous() {
        let dir = dir_with("{}");
        assert!(matches!(
            registry_auth(Some(dir.path()), "registry.example.com").unwrap(),
            RegistryAuth::Anonymous
        ));
    }

    #[test]
    fn a_directory_with_no_config_is_anonymous() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            registry_auth(Some(dir.path()), "registry.example.com").unwrap(),
            RegistryAuth::Anonymous
        ));
    }

    #[test]
    fn a_config_that_is_not_json_is_an_error_and_not_anonymous_access() {
        let dir = dir_with("this is not json");
        assert!(registry_auth(Some(dir.path()), "registry.example.com").is_err());
    }
}
