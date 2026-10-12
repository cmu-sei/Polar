//! What the Jira observer reads from its configuration, declared once.
//!
//! These were read with `std::env::var` where they were used, so nothing could list them,
//! check them before the agent started, or say what they were for. Declared here they are in the
//! agent's scaffold, judged with everything else, and the catalog cannot set a Jira variable that
//! this does not name.

use polar_env::{Config, ConfigError, Described, Optional, Required, Section};

/// The address of the Jira site.
pub static JIRA_URL: Required<String> = Required::text(
    "JIRA_URL",
    "the Jira site's address, for example https://example.atlassian.net",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JiraAuth, JiraDeployment};
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
