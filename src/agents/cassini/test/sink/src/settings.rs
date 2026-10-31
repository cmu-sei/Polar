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

//! What the sink reads beyond the run (`harness_common::settings`), declared once.

use polar_env::{Config, ConfigError, Defaulted, Described, Section};
use std::time::Duration;

pub static WARMUP: Defaulted<u64> = Defaulted::number(
    "HARNESS_WARMUP",
    "how many of each producer's first messages are left out of the latencies (they are still counted as received): \
     the first messages of a run measure connections and caches warming, not the broker",
    "0",
);

pub static IDLE_SECS: Defaulted<u64> = Defaulted::number(
    "HARNESS_IDLE_SECS",
    "how long the sink waits, once messages have started arriving, for the next one before it reports what it has",
    "30",
);

pub static FIRST_WAIT_SECS: Defaulted<u64> = Defaulted::number(
    "HARNESS_FIRST_WAIT_SECS",
    "how long the sink waits, once ready, for the first message before it reports that none came",
    "300",
);

/// A sink that is slow on purpose (docs/architecture/cassini-backpressure.md, "Tests, first"): how long it spends on
/// each message before the next, so a run can measure what a subscriber that falls behind costs the broker.
pub static DELAY_US: Defaulted<u64> = Defaulted::number(
    "HARNESS_SINK_DELAY_US",
    "how long the sink spends on each message it receives, in microseconds, before it takes the next; 0 spends \
     nothing",
    "0",
);

/// Where a slow sink spends its delay (docs/architecture/broker-baseline.md, the fifth set).
pub static SLOW: Defaulted<String> = Defaulted::text(
    "HARNESS_SINK_SLOW",
    "where a slow sink (HARNESS_SINK_DELAY_US) spends its delay: `after_ack`, after its client has handed the \
     message over and acknowledged it, so what waits for it waits in the sink and the broker never feels it; or \
     `before_ack`, in its client's reader before each delivery is handed over and acknowledged, so the \
     acknowledgements come at its pace and what waits for it waits in the broker",
    "after_ack",
);

static VARIABLES: &[&dyn Described] = &[&WARMUP, &IDLE_SECS, &FIRST_WAIT_SECS, &DELAY_US, &SLOW];

/// Where a slow sink spends its delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slow {
    /// In the sink's own handler, after the client acknowledged the delivery.
    AfterAck,
    /// In the client's reader, before the delivery is handed over and acknowledged
    /// (`cassini_client::hand_off`).
    BeforeAck,
}

impl Slow {
    fn parse(text: &str) -> Result<Self, ConfigError> {
        match text.trim() {
            "after_ack" => Ok(Slow::AfterAck),
            "before_ack" => Ok(Slow::BeforeAck),
            other => Err(ConfigError::Env(polar_env::EnvError {
                name: "HARNESS_SINK_SLOW",
                doc: "where a slow sink spends its delay",
                problem: polar_env::Problem::Malformed {
                    shown: other.to_string(),
                    reason: "must be after_ack or before_ack".to_string(),
                },
            })),
        }
    }
}

pub static SECTION: Section = Section {
    name: "harness-sink",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

pub struct SinkSettings {
    pub warmup: u64,
    pub idle: Duration,
    pub first_wait: Duration,
    /// Spent on each message; zero for none.
    pub delay: Duration,
    /// Where it is spent.
    pub slow: Slow,
}

impl SinkSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            warmup: config.read(&WARMUP)?,
            idle: Duration::from_secs(config.read(&IDLE_SECS)?),
            first_wait: Duration::from_secs(config.read(&FIRST_WAIT_SECS)?),
            delay: Duration::from_micros(config.read(&DELAY_US)?),
            slow: Slow::parse(&config.read(&SLOW)?)?,
        })
    }

    /// The delay the sink's own handler spends on each message: none when it is spent before the acknowledgement.
    pub fn handler_delay(&self) -> Duration {
        match self.slow {
            Slow::AfterAck => self.delay,
            Slow::BeforeAck => Duration::ZERO,
        }
    }

    /// The delay its client's reader spends on each delivery before handing it over and acknowledging it.
    pub fn hand_off_delay(&self) -> Duration {
        match self.slow {
            Slow::AfterAck => Duration::ZERO,
            Slow::BeforeAck => self.delay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn what_is_given_is_read() {
        let vars: HashMap<String, String> = [
            ("HARNESS_WARMUP", "100"),
            ("HARNESS_IDLE_SECS", "5"),
            ("HARNESS_FIRST_WAIT_SECS", "60"),
            ("HARNESS_SINK_DELAY_US", "250"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = SinkSettings::read(&config).unwrap();
        assert_eq!(s.warmup, 100);
        assert_eq!(s.idle, Duration::from_secs(5));
        assert_eq!(s.first_wait, Duration::from_secs(60));
        assert_eq!(s.delay, Duration::from_micros(250));
        assert_eq!(s.slow, Slow::AfterAck);
    }

    #[test]
    fn a_sink_is_not_slowed_unless_it_is_asked_to_be() {
        let vars: HashMap<String, String> = HashMap::new();
        let config = Config::builder(&vars).register(&SECTION).build();
        let s = SinkSettings::read(&config).unwrap();
        assert_eq!(s.delay, Duration::ZERO);
        assert_eq!(s.slow, Slow::AfterAck);
    }

    fn with_slow(slow: &str) -> Result<SinkSettings, ConfigError> {
        let vars: HashMap<String, String> = [
            ("HARNESS_SINK_DELAY_US", "250"),
            ("HARNESS_SINK_SLOW", slow),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        SinkSettings::read(&Config::builder(&vars).register(&SECTION).build())
    }

    /// The mode is read, and the delay goes to one place only: the sink's handler after the acknowledgement, or its
    /// client's reader before it.
    #[test]
    fn the_mode_says_where_the_delay_is_spent() {
        let after = with_slow("after_ack").unwrap();
        assert_eq!(after.slow, Slow::AfterAck);
        assert_eq!(after.handler_delay(), Duration::from_micros(250));
        assert_eq!(after.hand_off_delay(), Duration::ZERO);
        let before = with_slow("before_ack").unwrap();
        assert_eq!(before.slow, Slow::BeforeAck);
        assert_eq!(before.handler_delay(), Duration::ZERO);
        assert_eq!(before.hand_off_delay(), Duration::from_micros(250));
        let refused = with_slow("sometimes").err().unwrap().to_string();
        assert!(refused.contains("HARNESS_SINK_SLOW"), "{refused}");
        assert!(refused.contains("before_ack"), "{refused}");
    }
}
