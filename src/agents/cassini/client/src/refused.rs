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

//! The topics an agent was refused and still wants (docs/architecture/policy-enforcement.md, stage 5b). A refused
//! subscription is kept as wanted: it is not sent again each time the agent registers and subscribes again, only when
//! its gate is asked again, on a change of its authority, and admits it. A refused publish is remembered until then
//! too, so the agent's health can say what it was refused. The facade keeps one of these; it decides nothing here.

use crate::facade::TopicUse;
use std::collections::BTreeMap;

/// A topic the agent wants and was refused, as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingRefusal {
    pub topic: String,
    pub use_: TopicUse,
    /// Why, as the refusal said it.
    pub reason: String,
    /// The broker refused it, though the agent's own gate let it through.
    pub by_broker: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    reason: String,
    by_broker: bool,
}

/// The refused topics, by topic and use.
#[derive(Debug, Default, Clone)]
pub struct RefusedTopics {
    entries: BTreeMap<(String, TopicUse), Refusal>,
}

impl RefusedTopics {
    /// Notes that `use_` of `topic` was refused. Returns whether what stands changed.
    pub fn refuse(&mut self, topic: &str, use_: TopicUse, reason: &str, by_broker: bool) -> bool {
        let refusal = Refusal {
            reason: reason.to_string(),
            by_broker,
        };
        self.entries
            .insert((topic.to_string(), use_), refusal.clone())
            != Some(refusal)
    }

    /// `use_` of `topic` is refused no more: taken, admitted, or no longer wanted. Returns whether what stands
    /// changed.
    pub fn clear(&mut self, topic: &str, use_: TopicUse) -> bool {
        self.entries.remove(&(topic.to_string(), use_)).is_some()
    }

    /// Whether a subscription to `topic` is held back: refused, and not since admitted.
    pub fn holds_back(&self, topic: &str) -> bool {
        self.entries
            .contains_key(&(topic.to_string(), TopicUse::Subscribe))
    }

    /// What the gate is to be asked again about.
    pub fn refused(&self) -> Vec<(String, TopicUse)> {
        self.entries.keys().cloned().collect()
    }

    /// Takes the gate's new verdicts on what was refused: an admitted one is refused no more, and a refused one
    /// keeps the gate's reason. Returns the subscriptions now to send, and whether what stands changed. A topic
    /// with no verdict is left as it was.
    pub fn judged(
        &mut self,
        verdicts: &BTreeMap<(String, TopicUse), Result<(), String>>,
    ) -> (Vec<String>, bool) {
        let mut send = Vec::new();
        let mut changed = false;
        for ((topic, use_), verdict) in verdicts {
            match verdict {
                Ok(()) => {
                    if self.clear(topic, *use_) {
                        changed = true;
                        if *use_ == TopicUse::Subscribe {
                            send.push(topic.clone());
                        }
                    }
                }
                Err(reason) => {
                    if self.entries.contains_key(&(topic.clone(), *use_)) {
                        changed |= self.refuse(topic, *use_, reason, false);
                    }
                }
            }
        }
        (send, changed)
    }

    /// What stands, in order of topic.
    pub fn standing(&self) -> Vec<StandingRefusal> {
        self.entries
            .iter()
            .map(|((topic, use_), refusal)| StandingRefusal {
                topic: topic.clone(),
                use_: *use_,
                reason: refusal.reason.clone(),
                by_broker: refusal.by_broker,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    #[test]
    fn a_refused_subscription_is_held_back_until_the_gate_admits_it() {
        let mut refused = RefusedTopics::default();
        assert!(refused.refuse("news", TopicUse::Subscribe, "forbidden", false));
        assert!(refused.holds_back("news"));
        // Told again, the same: nothing changed.
        assert!(!refused.refuse("news", TopicUse::Subscribe, "forbidden", false));
        // A publish refused is no subscription held back.
        refused.refuse("weather", TopicUse::Publish, "forbidden", true);
        assert!(!refused.holds_back("weather"));

        let verdicts = BTreeMap::from([
            (("news".to_string(), TopicUse::Subscribe), Ok(())),
            (
                ("weather".to_string(), TopicUse::Publish),
                Err("still forbidden".to_string()),
            ),
        ]);
        let (send, changed) = refused.judged(&verdicts);
        assert_eq!(send, ["news"]);
        assert!(changed);
        assert!(!refused.holds_back("news"));
        // The gate's reason now, and the gate's refusal.
        assert_eq!(
            refused.standing(),
            [StandingRefusal {
                topic: "weather".to_string(),
                use_: TopicUse::Publish,
                reason: "still forbidden".to_string(),
                by_broker: false,
            }]
        );
    }

    #[test]
    fn a_subscription_no_longer_wanted_is_not_sent_when_admitted() {
        let mut refused = RefusedTopics::default();
        refused.refuse("news", TopicUse::Subscribe, "forbidden", true);
        assert!(refused.clear("news", TopicUse::Subscribe));
        let verdicts = BTreeMap::from([(("news".to_string(), TopicUse::Subscribe), Ok(()))]);
        assert_eq!(refused.judged(&verdicts), (vec![], false));
    }

    const TOPICS: &[&str] = &["news", "weather", "sport"];

    #[derive(Debug, Clone)]
    enum Op {
        /// The agent subscribes, as it does each time it registers.
        Subscribe(&'static str),
        Unsubscribe(&'static str),
        Publish(&'static str),
        /// The broker answers a subscription it was sent.
        Broker(&'static str, bool),
        /// The agent's authority changes: its gate admits these now.
        Authority(BTreeSet<&'static str>),
    }

    fn ops() -> impl Strategy<Value = Vec<Op>> {
        let topic = || proptest::sample::select(TOPICS);
        proptest::collection::vec(
            prop_oneof![
                topic().prop_map(Op::Subscribe),
                topic().prop_map(Op::Unsubscribe),
                topic().prop_map(Op::Publish),
                (topic(), any::<bool>()).prop_map(|(t, ok)| Op::Broker(t, ok)),
                proptest::collection::btree_set(topic(), 0..=3).prop_map(Op::Authority),
            ],
            0..30,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Driven as the facade drives it, for any sequence of subscribes, unsubscribes, publishes, broker answers and
        /// changes of authority: every subscription the agent wants is either sent or held back, never both and never
        /// neither; one is sent only when the gate admits it at that moment; after a change of authority nothing the
        /// gate admits is still refused; and only what the agent still wants stands refused.
        #[test]
        fn every_wanted_subscription_is_sent_or_held_back_and_only_what_the_gate_admits_is_sent(ops in ops()) {
            let mut refused = RefusedTopics::default();
            let mut admitted: BTreeSet<&str> = BTreeSet::new();
            let mut wanted: BTreeSet<&str> = BTreeSet::new();
            // Sent to the broker, and not refused by it or given up since.
            let mut sent: BTreeSet<String> = BTreeSet::new();
            let gate = |admitted: &BTreeSet<&str>, topic: &str| {
                if admitted.contains(topic) { Ok(()) } else { Err(format!("{topic} is not admitted")) }
            };
            for op in ops {
                match op {
                    Op::Subscribe(topic) => {
                        wanted.insert(topic);
                        if refused.holds_back(topic) || sent.contains(topic) {
                            continue;
                        }
                        match gate(&admitted, topic) {
                            Ok(()) => { sent.insert(topic.to_string()); }
                            Err(why) => { refused.refuse(topic, TopicUse::Subscribe, &why, false); }
                        }
                    }
                    Op::Unsubscribe(topic) => {
                        wanted.remove(topic);
                        sent.remove(topic);
                        refused.clear(topic, TopicUse::Subscribe);
                    }
                    Op::Publish(topic) => {
                        if let Err(why) = gate(&admitted, topic) {
                            refused.refuse(topic, TopicUse::Publish, &why, false);
                        }
                    }
                    Op::Broker(topic, ok) => {
                        // Only what was sent is answered.
                        if !sent.contains(topic) {
                            continue;
                        }
                        if ok {
                            refused.clear(topic, TopicUse::Subscribe);
                        } else {
                            sent.remove(topic);
                            refused.refuse(topic, TopicUse::Subscribe, "the broker refused", true);
                        }
                    }
                    Op::Authority(now) => {
                        admitted = now;
                        let verdicts = refused
                            .refused()
                            .into_iter()
                            .map(|(topic, use_)| {
                                let verdict = gate(&admitted, &topic);
                                ((topic, use_), verdict)
                            })
                            .collect();
                        let (send, _) = refused.judged(&verdicts);
                        for topic in send {
                            prop_assert!(admitted.contains(topic.as_str()), "sent {topic}, not admitted");
                            sent.insert(topic);
                        }
                        for standing in refused.standing() {
                            prop_assert!(!admitted.contains(standing.topic.as_str()), "{standing:?} admitted");
                        }
                    }
                }
                for topic in &wanted {
                    prop_assert!(
                        sent.contains(*topic) != refused.holds_back(topic),
                        "{topic}: sent {}, held back {}", sent.contains(*topic), refused.holds_back(topic)
                    );
                }
                for standing in refused.standing() {
                    if standing.use_ == TopicUse::Subscribe {
                        prop_assert!(wanted.contains(standing.topic.as_str()), "{standing:?} not wanted");
                    }
                }
            }
        }
    }
}
