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

//! What the producer reads beyond the run (`harness_common::settings`), declared once.

use harness_common::settings::{INDEX, PRODUCERS};
use polar_env::{Config, ConfigError, Defaulted, Described, Env, Section};
use std::time::Duration;

pub static MESSAGE_SIZE: Defaulted<u64> = Defaulted::number(
    "HARNESS_MESSAGE_SIZE",
    "the payload of each message, in bytes, after the harness's 20-byte header",
    "256",
);

pub static RATE: Defaulted<u64> = Defaulted::number(
    "HARNESS_RATE",
    "messages a second; 0 sends as fast as the window allows",
    "1000",
);

pub static WINDOW_BYTES: Defaulted<u64> = Defaulted::number(
    "HARNESS_WINDOW_BYTES",
    "the most payload sent and not yet acknowledged by the broker, in bytes (at least one message): a producer that \
     outruns the broker falls behind its rate instead of holding the run in memory",
    "67108864",
);

pub static WINDOW_MESSAGES: Defaulted<u64> = Defaulted::number(
    "HARNESS_WINDOW",
    "the most messages sent and not yet acknowledged by the broker (HARNESS_WINDOW_BYTES caps it too): small enough that \
     a saturated run measures the broker, not a queue the producer built in front of it",
    "1000",
);

pub static STALL_SECS: Defaulted<u64> = Defaulted::number(
    "HARNESS_STALL_SECS",
    "how long the producer waits for an acknowledgement before it gives up on the rest, and reports what it has",
    "30",
);

static VARIABLES: &[&dyn Described] = &[
    &MESSAGE_SIZE,
    &RATE,
    &WINDOW_MESSAGES,
    &WINDOW_BYTES,
    &STALL_SECS,
];

fn relations(env: &dyn Env) -> Vec<String> {
    match (INDEX.read(env), PRODUCERS.read(env)) {
        (Ok(index), Ok(producers)) if index >= producers => vec![format!(
            "HARNESS_INDEX is {index}, but the run has {producers} producer(s), numbered from 0"
        )],
        _ => Vec::new(),
    }
}

pub static SECTION: Section = Section {
    name: "harness-producer",
    vars: VARIABLES,
    relations,
};

pub struct ProducerSettings {
    pub size: u64,
    pub rate: u64,
    pub window_messages: u64,
    pub window_bytes: u64,
    pub stall: Duration,
}

impl ProducerSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            size: config.read(&MESSAGE_SIZE)?,
            rate: config.read(&RATE)?,
            window_messages: config.read(&WINDOW_MESSAGES)?,
            window_bytes: config.read(&WINDOW_BYTES)?,
            stall: Duration::from_secs(config.read(&STALL_SECS)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn a_producer_past_the_run_s_count_is_refused() {
        let problems = SECTION.check(&env(&[("HARNESS_INDEX", "2"), ("HARNESS_PRODUCERS", "2")]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("numbered from 0"), "{problems:?}");
        assert!(
            SECTION
                .check(&env(&[("HARNESS_INDEX", "1"), ("HARNESS_PRODUCERS", "2")]))
                .is_empty()
        );
    }

    #[test]
    fn what_is_given_is_read() {
        let vars = env(&[
            ("HARNESS_MESSAGE_SIZE", "4096"),
            ("HARNESS_RATE", "0"),
            ("HARNESS_WINDOW", "10"),
            ("HARNESS_WINDOW_BYTES", "1024"),
            ("HARNESS_STALL_SECS", "5"),
        ]);
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = ProducerSettings::read(&config).unwrap();
        assert_eq!(
            (s.size, s.rate, s.window_messages, s.window_bytes),
            (4096, 0, 10, 1024)
        );
        assert_eq!(s.stall, Duration::from_secs(5));
    }
}
