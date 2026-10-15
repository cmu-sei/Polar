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

//! What the scheduler observer reads from its configuration, declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Optional, Section};

pub static LOCAL_PATH: Defaulted<String> = Defaulted::text(
    "POLAR_SCHEDULER_LOCAL_PATH",
    "where the schedules repository is checked out; the observer watches this directory",
    "/tmp/polar-schedules",
);

pub static REMOTE_URL: Optional<String> = Optional::text(
    "POLAR_SCHEDULER_REMOTE_URL",
    "the git remote the schedules are cloned from and kept in step with; unset, the observer \
     watches the local directory only",
);

pub static SYNC_INTERVAL: Optional<u64> = Optional::number(
    "POLAR_SCHEDULER_SYNC_INTERVAL",
    "seconds between pulls of the remote; 0 or unset means only a change announced by the \
     Polar git server triggers one. A remote that is not the Polar git server announces nothing, \
     so it needs an interval",
);

pub static GIT_CA_CERT: Optional<String> = Optional::text(
    "POLAR_SCHEDULER_GIT_CA_CERT",
    "path to the CA certificate (PEM) the git remote's certificate is verified against; unset, \
     git uses its usual trust",
);

pub static GIT_CLIENT_CERT: Optional<String> = Optional::text(
    "POLAR_SCHEDULER_GIT_CLIENT_CERT",
    "path to the certificate (PEM) presented to the git remote, which is how the Polar git \
     server knows who is asking; set together with POLAR_SCHEDULER_GIT_CLIENT_KEY",
);

pub static GIT_CLIENT_KEY: Optional<String> = Optional::text(
    "POLAR_SCHEDULER_GIT_CLIENT_KEY",
    "path to the private key (PEM) for POLAR_SCHEDULER_GIT_CLIENT_CERT",
);

static VARIABLES: &[&dyn Described] = &[
    &LOCAL_PATH,
    &REMOTE_URL,
    &SYNC_INTERVAL,
    &GIT_CA_CERT,
    &GIT_CLIENT_CERT,
    &GIT_CLIENT_KEY,
];

pub static SECTION: Section = Section {
    name: "polar-scheduler-observer",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct ObserverSettings {
    pub local_path: String,
    pub remote_url: Option<String>,
    pub sync_interval_secs: Option<u64>,
    pub git_ca_cert: Option<String>,
    pub git_client_cert: Option<String>,
    pub git_client_key: Option<String>,
}

impl ObserverSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            local_path: config.read(&LOCAL_PATH)?,
            remote_url: config.read(&REMOTE_URL)?,
            sync_interval_secs: config.read(&SYNC_INTERVAL)?,
            git_ca_cert: config.read(&GIT_CA_CERT)?,
            git_client_cert: config.read(&GIT_CLIENT_CERT)?,
            git_client_key: config.read(&GIT_CLIENT_KEY)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let env: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::builder(&env).register(&SECTION).build()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn nothing_is_required_and_the_path_has_a_default() {
        let settings = ObserverSettings::read(&config(&[])).unwrap();
        assert_eq!(settings.local_path, "/tmp/polar-schedules");
        assert_eq!(settings.remote_url, None);
        assert_eq!(settings.sync_interval_secs, None);
        assert_eq!(settings.git_client_cert, None);
        assert!(config(&[]).check().is_empty());
    }

    #[test]
    fn what_is_set_is_used() {
        let settings = ObserverSettings::read(&config(&[
            ("POLAR_SCHEDULER_LOCAL_PATH", "/data/schedules"),
            (
                "POLAR_SCHEDULER_REMOTE_URL",
                "https://git.example/polar-schedules.git",
            ),
            ("POLAR_SCHEDULER_SYNC_INTERVAL", "60"),
            ("POLAR_SCHEDULER_GIT_CA_CERT", "/c/ca.pem"),
            ("POLAR_SCHEDULER_GIT_CLIENT_CERT", "/c/cert.pem"),
            ("POLAR_SCHEDULER_GIT_CLIENT_KEY", "/c/key.pem"),
        ]))
        .unwrap();
        assert_eq!(settings.local_path, "/data/schedules");
        assert_eq!(settings.sync_interval_secs, Some(60));
        assert_eq!(settings.git_client_key.as_deref(), Some("/c/key.pem"));
    }

    #[test]
    fn an_interval_that_is_not_a_number_is_a_problem_before_the_agent_starts() {
        let problems = config(&[("POLAR_SCHEDULER_SYNC_INTERVAL", "soon")]).check();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("POLAR_SCHEDULER_SYNC_INTERVAL"),
            "{problems:?}"
        );
    }
}
