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

//! What the Jira observer reads from its configuration, declared once.
//!
//! These were read with `std::env::var` where they were used, so nothing could list them,
//! check them before the agent started, or say what they were for. Declared here they are in the
//! agent's scaffold, judged with everything else, and the catalog cannot set a Jira variable that
//! this does not name.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use polar_env::{Config, ConfigError, Described, Optional, Required, Section};
use url::Url;

/// The address of the Jira site.
pub static JIRA_URL: Required<String> = Required::text(
    "JIRA_URL",
    "the Jira site's address, for example https://jira.example.org (a context path such as https://host/jira is kept)",
);

/// The credential the observer presents to Jira.
pub static JIRA_TOKEN: Required<String> = Required::text(
    "JIRA_TOKEN",
    "the API token (Atlassian Cloud) or the bearer token (Server, Data Center) the observer presents to Jira",
)
.secret();

pub static JIRA_EMAIL: Optional<String> = Optional::text(
    "JIRA_EMAIL",
    "the account email, which with the token makes Atlassian Cloud's basic authentication; leave it unset for a bearer token",
);

pub static JIRA_DEPLOYMENT: Optional<String> = Optional::text(
    "JIRA_DEPLOYMENT",
    "cloud, server or datacenter; when unset it follows the address (an atlassian.net site is cloud)",
);

pub static PROXY_CA_CERT: Optional<String> = Optional::text(
    "PROXY_CA_CERT",
    "path to a CA certificate to trust as well as the system's, for a proxy that intercepts TLS",
);

pub static QUERY_DATE: Optional<String> = Optional::text(
    "QUERY_DATE",
    "a date from which the first full pull asks for issues (updated > date); unset pulls all of them",
);

static VARIABLES: &[&dyn Described] = &[
    &JIRA_URL,
    &JIRA_TOKEN,
    &JIRA_EMAIL,
    &JIRA_DEPLOYMENT,
    &PROXY_CA_CERT,
    &QUERY_DATE,
];

/// The section the Jira observer registers.
pub static SECTION: Section = Section {
    name: "jira",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// The observer's settings, read from the snapshot the builder took.
pub struct JiraSettings {
    pub url: String,
    pub token: String,
    pub email: Option<String>,
    pub deployment: Option<String>,
    pub proxy_ca_cert: Option<String>,
    pub query_date: Option<String>,
}

impl JiraSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            url: config.read(&JIRA_URL)?,
            token: config.read(&JIRA_TOKEN)?,
            email: config.read(&JIRA_EMAIL)?,
            deployment: config.read(&JIRA_DEPLOYMENT)?,
            proxy_ca_cert: config.read(&PROXY_CA_CERT)?,
            query_date: config.read(&QUERY_DATE)?,
        })
    }
}

/// The host the target templates ship in JIRA_URL until someone sets the real one.
const PLACEHOLDER_HOST: &str = "example.atlassian.net";

/// JIRA_URL as the observers use it: the site's address with no trailing slash, so that an
/// endpoint is the address followed by its path. A context path (`https://host/jira`) is kept.
/// Refused, with what to set instead: an address that is not http or https, one with the REST
/// path pasted in (`/rest/api/2`), one with a query or a fragment, and the placeholder.
pub fn normalise_jira_url(raw: &str) -> Result<String, String> {
    const EXAMPLE: &str =
        "for example https://jira.example.org, or https://host/jira with a context path";

    let url = Url::parse(raw.trim()).map_err(|e| {
        format!("JIRA_URL is not an address ({e}): set it to the site's https:// or http:// address, {EXAMPLE}")
    })?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err(format!(
            "JIRA_URL must start with https:// or http://, not {}://: set it to the site's address, {EXAMPLE}",
            url.scheme()
        ));
    }
    if url
        .path_segments()
        .is_some_and(|mut segments| segments.any(|s| s.eq_ignore_ascii_case("rest")))
    {
        return Err(format!(
            "JIRA_URL must be the site's address without the REST path: drop /rest and what follows it, {EXAMPLE}"
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!(
            "JIRA_URL must be the site's address with no query (?) or fragment (#), {EXAMPLE}"
        ));
    }
    if url.host_str() == Some(PLACEHOLDER_HOST) {
        return Err(format!(
            "JIRA_URL is still the example placeholder https://{PLACEHOLDER_HOST}: \
             set workloads.jira-observer.env in the target's config.nuon"
        ));
    }

    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// True if the token looks like a base64-encoded copy of a token rather than the token itself:
/// it decodes, as standard or URL-safe base64 with or without padding, to at least 16
/// characters of printable ASCII with no whitespace. A Data Center personal access token is
/// base64 too, but of binary, so it does not pass for one.
pub fn token_looks_base64(token: &str) -> bool {
    let token = token.trim();
    [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .filter_map(|engine| engine.decode(token).ok())
        .any(|decoded| decoded.len() >= 16 && decoded.iter().all(u8::is_ascii_graphic))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JiraAuth, JiraDeployment};
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

    #[test]
    fn trailing_slashes_are_dropped_and_a_context_path_is_kept() {
        let cases = [
            ("https://jira.example.org", "https://jira.example.org"),
            ("https://jira.example.org/", "https://jira.example.org"),
            ("https://jira.example.org///", "https://jira.example.org"),
            (
                "https://host.example.org/jira",
                "https://host.example.org/jira",
            ),
            (
                "https://host.example.org/jira//",
                "https://host.example.org/jira",
            ),
            (
                "http://jira.example.org:8080/",
                "http://jira.example.org:8080",
            ),
            (" https://jira.example.org/\n", "https://jira.example.org"),
            // Only a segment that is "rest" is the REST path.
            (
                "https://jira.example.org/restricted",
                "https://jira.example.org/restricted",
            ),
        ];
        for (given, expected) in cases {
            assert_eq!(
                normalise_jira_url(given).as_deref(),
                Ok(expected),
                "{given:?}"
            );
        }
    }

    #[test]
    fn an_address_that_is_not_http_or_https_is_refused_with_what_to_set() {
        for given in [
            "jira.example.org",
            "jira.example.org:8443",
            "ftp://jira.example.org",
            "file:///etc/passwd",
        ] {
            let error = normalise_jira_url(given).unwrap_err();
            assert!(
                error.contains("JIRA_URL") && error.contains("https://"),
                "{given:?}: {error}"
            );
        }
    }

    #[test]
    fn a_rest_path_pasted_into_the_address_is_refused() {
        for given in [
            "https://jira.example.org/rest/api/2",
            "https://jira.example.org/rest/api/2/",
            "https://jira.example.org/rest",
            "https://host.example.org/jira/rest/api/latest/project",
            "https://jira.example.org/REST/api/2",
        ] {
            let error = normalise_jira_url(given).unwrap_err();
            assert!(
                error.contains("JIRA_URL") && error.contains("/rest"),
                "{given:?}: {error}"
            );
        }
    }

    #[test]
    fn a_query_or_a_fragment_is_refused() {
        for given in [
            "https://jira.example.org/?os_authType=basic",
            "https://jira.example.org/#top",
        ] {
            assert!(normalise_jira_url(given).is_err(), "{given:?}");
        }
    }

    #[test]
    fn the_placeholder_is_refused_with_where_to_set_the_real_address() {
        let expected = "JIRA_URL is still the example placeholder https://example.atlassian.net: \
                        set workloads.jira-observer.env in the target's config.nuon";
        for given in [
            "https://example.atlassian.net",
            "https://example.atlassian.net/",
            "https://EXAMPLE.atlassian.net",
        ] {
            assert_eq!(
                normalise_jira_url(given),
                Err(expected.to_string()),
                "{given:?}"
            );
        }
        assert!(normalise_jira_url("https://mysite.atlassian.net").is_ok());
    }

    #[test]
    fn a_base64_copy_of_a_token_is_noticed() {
        let raw = "ATATT3xFfGF0Qm9ndXNUb2tlbkZvclRlc3Rpbmc=A1B2C3D4";
        assert!(!token_looks_base64(raw), "the raw token itself");
        assert!(token_looks_base64(&STANDARD.encode(raw)));
        assert!(token_looks_base64(&URL_SAFE_NO_PAD.encode(raw)));
        assert!(token_looks_base64(&format!("{}\n", STANDARD.encode(raw))));
    }

    #[test]
    fn a_data_center_token_or_a_short_one_is_not_taken_for_a_copy() {
        // A Data Center personal access token: 44 characters of base64, of an id, a colon and
        // random bytes.
        let mut pat = b"471706345638:".to_vec();
        pat.extend([
            0xb0, 0x58, 0x1f, 0x9c, 0x02, 0xe7, 0x44, 0x81, 0x3a, 0xff, 0x10, 0x6d, 0xc2, 0x99,
            0x07, 0x5e, 0xa1, 0x30, 0x8b, 0xd4,
        ]);
        let pat = STANDARD.encode(&pat);
        assert_eq!(pat.len(), 44);
        assert!(!token_looks_base64(&pat));
        // Too short to say: "short-token" decodes to fewer than 16 characters.
        assert!(!token_looks_base64(&STANDARD.encode("short-token")));
        // Whitespace in what it decodes to is not a token.
        assert!(!token_looks_base64(
            &STANDARD.encode("a phrase with spaces in it")
        ));
        assert!(!token_looks_base64("t0ken"));
    }
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let env: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::builder(&env).register(&SECTION).build()
    }

    const MINIMUM: &[(&str, &str)] = &[
        ("JIRA_URL", "https://jira.example.org"),
        ("JIRA_TOKEN", "t0ken"),
    ];

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn a_url_and_a_token_are_enough() {
        let settings = JiraSettings::read(&config(MINIMUM)).unwrap();
        assert_eq!(settings.url, "https://jira.example.org");
        assert_eq!(settings.token, "t0ken");
        assert_eq!(settings.email, None);
        assert_eq!(settings.deployment, None);
        assert_eq!(settings.proxy_ca_cert, None);
        assert_eq!(settings.query_date, None);
    }

    #[test]
    fn a_missing_url_or_token_is_named_with_what_it_is_for() {
        let error = config(&[]).check().join(" | ");
        assert!(
            error.contains("JIRA_URL") && error.contains("address"),
            "{error}"
        );
        assert!(error.contains("JIRA_TOKEN"), "{error}");
        assert!(JiraSettings::read(&config(&[])).is_err());
    }

    #[test]
    fn the_token_is_never_shown_in_a_report() {
        let c = config(MINIMUM);
        let report = c.report();
        let token = report.iter().find(|r| r.name == "JIRA_TOKEN").unwrap();
        assert_eq!(token.value.as_deref(), Some("<hidden>"));
    }

    #[test]
    fn an_email_makes_basic_authentication_and_no_email_a_bearer_token() {
        assert!(matches!(
            JiraAuth::from_parts(Some("me@example.org".into()), "t".into()),
            JiraAuth::Basic { .. }
        ));
        assert!(matches!(
            JiraAuth::from_parts(None, "t".into()),
            JiraAuth::Bearer { .. }
        ));
        assert!(
            matches!(
                JiraAuth::from_parts(Some("".into()), "t".into()),
                JiraAuth::Bearer { .. }
            ),
            "an empty email is no email"
        );
    }

    #[test]
    fn the_deployment_follows_what_is_said_and_otherwise_the_address() {
        use JiraDeployment::*;
        let url = "https://jira.example.org";
        assert!(JiraDeployment::resolve(Some("cloud"), url) == Cloud);
        assert!(JiraDeployment::resolve(Some("CLOUD"), url) == Cloud);
        assert!(
            JiraDeployment::resolve(Some("server"), "https://x.atlassian.net")
                == ServerOrDataCenter
        );
        assert!(JiraDeployment::resolve(Some("datacenter"), url) == ServerOrDataCenter);
        assert!(JiraDeployment::resolve(None, "https://x.atlassian.net") == Cloud);
        assert!(JiraDeployment::resolve(None, url) == ServerOrDataCenter);
        assert!(JiraDeployment::resolve(Some("nonsense"), url) == ServerOrDataCenter);
    }
}
