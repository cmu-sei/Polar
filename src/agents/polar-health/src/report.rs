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

//! Pulled health (docs/architecture/soft-state.md, "As built: pulled health"): what the health actor answers when its
//! agent is asked, and the words on the wire.
//!
//! Every agent hears a question on its own topic, [`health_topic`] (`agent.<name>.health`, implied by the `agent`
//! rule), and answers on [`HEALTH_REPLIES_TOPIC`], which only the state agent's writer subscribes to. The answer is the
//! actor's [`HealthReport`]: a status, the phase, a [`HealthReason`] and a [`sanitize`]d one-line message, the
//! connections, the counters and the uptime. Nothing in it comes from looking into a domain actor: a domain makes a
//! standing problem known only by telling the actor ([`HoldReason`], through the builder's `HealthHandle::hold`).

/// Where every agent answers, and only the state agent's writer listens.
pub const HEALTH_REPLIES_TOPIC: &str = "polar-health-replies";

/// The family the state agent's writer asks on, one topic for each workload it serves.
pub const HEALTH_QUERY_FAMILY: &str = "agent.{workload}.health";

/// The topic the agent `name` is asked on.
pub fn health_topic(name: &str) -> String {
    HEALTH_QUERY_FAMILY.replacen("{workload}", name, 1)
}

/// The longest message a report carries, in characters.
pub const MAX_MESSAGE_CHARS: usize = 200;

/// The shortest secret value [`sanitize`] looks for: a shorter one would match ordinary words and say nothing.
pub const MIN_SECRET_CHARS: usize = 4;

/// What stands in a message where something was taken out.
pub const WITHHELD: &str = "<withheld>";

/// What a report says of the agent as a whole.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// Not yet ready for the first time: its connections have never all been up.
    Starting,
    Healthy,
    /// Up and answering, with something it does without (the graph, for the state agent's reader).
    Degraded,
    /// It cannot do its job: a connection it needs is down, or it holds a standing problem.
    Unhealthy,
    /// It was told to end and is draining.
    Draining,
}

impl ReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportStatus::Starting => "starting",
            ReportStatus::Healthy => "healthy",
            ReportStatus::Degraded => "degraded",
            ReportStatus::Unhealthy => "unhealthy",
            ReportStatus::Draining => "draining",
        }
    }
}

/// Why a report is not healthy, split by whether a restart helps.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    /// The Cassini session. A restart helps: the verdict ends the agent.
    Broker,
    /// The graph. A restart helps: the verdict ends the agent.
    Graph,
    /// A dependency's endpoint or certificate. A restart helps: the verdict ends the agent.
    Dependency,
    /// The agent's own machinery (its certificate, its health file, its custody). A restart helps.
    Internal,
    /// Its configuration cannot work. A restart does not help: the agent stays up, reports and stops retrying.
    Configuration,
    /// Its credentials are refused. A restart does not help: the agent stays up, reports and stops retrying.
    Credentials,
}

impl HealthReason {
    /// Whether ending the agent and starting it again can help.
    pub fn restart_helps(self) -> bool {
        !matches!(
            self,
            HealthReason::Configuration | HealthReason::Credentials
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            HealthReason::Broker => "broker",
            HealthReason::Graph => "graph",
            HealthReason::Dependency => "dependency",
            HealthReason::Internal => "internal",
            HealthReason::Configuration => "configuration",
            HealthReason::Credentials => "credentials",
        }
    }
}

/// A standing problem a domain reports: one a restart does not help. It never becomes a verdict, so the agent stays
/// up, answers health queries and says why it does nothing; the domain stops retrying until it is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// Its configuration cannot work (an address that is a placeholder, a value it cannot use).
    Configuration,
    /// Its credentials are refused (a token the site rejects).
    Credentials,
}

impl From<HoldReason> for HealthReason {
    fn from(reason: HoldReason) -> Self {
        match reason {
            HoldReason::Configuration => HealthReason::Configuration,
            HoldReason::Credentials => HealthReason::Credentials,
        }
    }
}

/// The health actor's own counts, since the agent started.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct HealthCounters {
    pub cassini_disconnects: u64,
    pub graph_op_failures: u64,
    /// The topics it wants and is refused now.
    pub refused_topics: u64,
    /// The health file writes that failed in a row, now.
    pub health_file_write_failures: u64,
}

/// What the health actor answers: its snapshot of the agent.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct HealthReport {
    pub status: ReportStatus,
    pub phase: crate::HealthPhase,
    pub reason: Option<HealthReason>,
    /// One line, at most [`MAX_MESSAGE_CHARS`], never a secret: see [`sanitize`].
    pub message: Option<String>,
    pub cassini_connected: bool,
    pub graph_connected: bool,
    pub expects_graph: bool,
    pub counters: HealthCounters,
    pub uptime_secs: u64,
}

/// The question, on the asked agent's own topic.
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct HealthQuery {
    /// The asker's round: the answer carries it back.
    pub correlation_id: String,
}

/// The answer, on [`HEALTH_REPLIES_TOPIC`]. Who answered is the broker's word (`published_by`), never the payload's.
#[derive(Debug, Clone, PartialEq, Eq, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
pub struct HealthAnswer {
    pub correlation_id: String,
    pub report: HealthReport,
}

pub fn encode_query(query: &HealthQuery) -> Vec<u8> {
    rkyv::to_bytes::<rkyv::rancor::Error>(query)
        .expect("a query of one string always serializes")
        .into_vec()
}

pub fn encode_answer(answer: &HealthAnswer) -> Vec<u8> {
    rkyv::to_bytes::<rkyv::rancor::Error>(answer)
        .expect("an answer of owned strings, numbers and enums always serializes")
        .into_vec()
}

/// The query in `payload`, or why it is not one. The bytes are validated before anything is read from them.
pub fn decode_query(payload: &[u8]) -> Result<HealthQuery, String> {
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(payload);
    rkyv::from_bytes::<HealthQuery, rkyv::rancor::Error>(&aligned)
        .map_err(|e| format!("not a health query: {e}"))
}

/// The answer in `payload`, or why it is not one.
pub fn decode_answer(payload: &[u8]) -> Result<HealthAnswer, String> {
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(payload);
    rkyv::from_bytes::<HealthAnswer, rkyv::rancor::Error>(&aligned)
        .map_err(|e| format!("not a health answer: {e}"))
}

/// Names whose value, written `name=value` or `name: value`, is a credential.
const CREDENTIAL_NAMES: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "credential",
    "private_key",
];

/// Schemes whose next word is a credential.
const CREDENTIAL_SCHEMES: &[&str] = &["bearer ", "basic "];

/// A message as a report may carry it, or none when it cannot be made safe: one line (every run of whitespace and
/// control characters one space), each value of `secrets` at least [`MIN_SECRET_CHARS`] long taken out, and the shapes
/// a credential takes withheld (a PEM block, a URL's user information, the word after `Bearer` or `Basic`, the value
/// of a name such as `password=` or `token:`), then cut to [`MAX_MESSAGE_CHARS`]. An empty result is none.
pub fn sanitize(message: &str, secrets: &[&str]) -> Option<String> {
    let secrets: Vec<&str> = secrets
        .iter()
        .copied()
        .filter(|s| s.chars().count() >= MIN_SECRET_CHARS)
        .collect();
    let mut text = withhold_secrets(message, &secrets);
    text = one_line(&text);
    text = withhold_pem(&text);
    text = withhold_userinfo(&text);
    text = withhold_after_schemes(&text);
    text = withhold_assignments(&text);
    // The one-line form of a secret, too: a secret with a newline in it reads with a space now.
    let normalised: Vec<String> = secrets.iter().map(|s| one_line(s)).collect();
    let normalised: Vec<&str> = normalised
        .iter()
        .map(String::as_str)
        .filter(|s| s.chars().count() >= MIN_SECRET_CHARS)
        .collect();
    for _ in 0..4 {
        let before = text.clone();
        text = withhold_secrets(&text, &secrets);
        text = withhold_secrets(&text, &normalised);
        if text == before {
            break;
        }
    }
    // Taking something out can never be allowed to leave it in: a message that still holds a secret is not sent.
    if secrets
        .iter()
        .chain(normalised.iter())
        .any(|s| text.contains(s))
    {
        return None;
    }
    let text = cut(text.trim());
    (!text.is_empty()).then_some(text)
}

fn withhold_secrets(text: &str, secrets: &[&str]) -> String {
    let mut text = text.to_string();
    for secret in secrets {
        if text.contains(secret) {
            text = text.replace(secret, WITHHELD);
        }
    }
    text
}

fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        }
    }
    out
}

/// Everything from a PEM header on.
fn withhold_pem(text: &str) -> String {
    match text.find("-----BEGIN") {
        Some(at) => format!("{}{WITHHELD}", &text[..at]),
        None => text.to_string(),
    }
}

/// The user information of every URL: `scheme://user:password@host` becomes `scheme://<withheld>@host`.
fn withhold_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(user_end) => {
                out.push_str(WITHHELD);
                out.push_str(&authority[user_end..]);
            }
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Byte ranges of the word starting at `from`: up to whitespace, a quote, a comma or a semicolon.
fn word_end(text: &str, from: usize) -> usize {
    text[from..]
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'))
        .map_or(text.len(), |n| from + n)
}

/// The word after `Bearer ` or `Basic `, in any case.
fn withhold_after_schemes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    // ASCII lower-casing keeps every byte where it was, so an offset in one is an offset in the other.
    let lower = text.to_ascii_lowercase();
    while i < text.len() {
        let found = CREDENTIAL_SCHEMES
            .iter()
            .filter_map(|scheme| lower[i..].find(scheme).map(|n| (i + n, scheme.len())))
            .min();
        let Some((at, len)) = found else { break };
        let value = at + len;
        let end = word_end(text, value);
        out.push_str(&text[i..value]);
        if end > value {
            out.push_str(WITHHELD);
        }
        i = end;
    }
    out.push_str(&text[i..]);
    out
}

/// The value of every `name=value` or `name: value` whose name holds a credential's (`JIRA_TOKEN=...`,
/// `password: ...`).
fn withhold_assignments(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0;
    while i < text.len() {
        let found = CREDENTIAL_NAMES
            .iter()
            .filter_map(|name| lower[i..].find(name).map(|n| (i + n, name.len())))
            .min();
        let Some((at, len)) = found else { break };
        // The rest of the name (`token_file`, `secrets`), then spaces, then the separator.
        let mut j = at + len;
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'_' | b'-'))
        {
            j += 1;
        }
        while j < bytes.len() && bytes[j] == b' ' {
            j += 1;
        }
        if j < bytes.len() && matches!(bytes[j], b'=' | b':') {
            j += 1;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'"' | b'\'') {
                j += 1;
            }
            let end = word_end(text, j);
            if end > j && &text[j..end] != WITHHELD {
                out.push_str(&text[copied..j]);
                out.push_str(WITHHELD);
                copied = end;
            }
            i = end.max(j);
        } else {
            i = j.max(at + 1);
        }
        // `i` may sit inside a multi-byte character only if a name matched there, and names are ASCII.
        while !text.is_char_boundary(i) {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// At most [`MAX_MESSAGE_CHARS`] characters, the last an ellipsis when something was cut.
fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_MESSAGE_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn each_agent_is_asked_on_its_own_topic() {
        assert_eq!(health_topic("jira-observer"), "agent.jira-observer.health");
        assert_ne!(health_topic("a"), health_topic("b"));
    }

    #[test]
    fn only_configuration_and_credentials_are_problems_a_restart_does_not_help() {
        for reason in [
            HealthReason::Broker,
            HealthReason::Graph,
            HealthReason::Dependency,
            HealthReason::Internal,
        ] {
            assert!(reason.restart_helps(), "{reason:?}");
        }
        for hold in [HoldReason::Configuration, HoldReason::Credentials] {
            assert!(!HealthReason::from(hold).restart_helps(), "{hold:?}");
        }
    }

    #[test]
    fn a_message_is_one_line_and_its_credentials_are_withheld() {
        assert_eq!(
            sanitize("JIRA_URL is still the example placeholder\n  set it", &[]).as_deref(),
            Some("JIRA_URL is still the example placeholder set it")
        );
        assert_eq!(
            sanitize("refused https://ann:hunter22@jira.example/rest", &[]).as_deref(),
            Some("refused https://<withheld>@jira.example/rest")
        );
        assert_eq!(
            sanitize("sent Authorization: Bearer abc.def.ghi", &[]).as_deref(),
            Some("sent Authorization: <withheld> <withheld>")
        );
        assert_eq!(
            sanitize("JIRA_TOKEN=abcd1234 and password: 'p4ss'", &[]).as_deref(),
            Some("JIRA_TOKEN=<withheld> and password: '<withheld>'")
        );
        assert_eq!(
            sanitize("key -----BEGIN PRIVATE KEY-----\nMIIE", &[]).as_deref(),
            Some("key <withheld>")
        );
        // A word that only mentions a credential is left alone.
        assert_eq!(
            sanitize("JIRA_TOKEN is the credential", &[]).as_deref(),
            Some("JIRA_TOKEN is the credential")
        );
        assert_eq!(
            sanitize("the token s3cr3t-value was refused", &["s3cr3t-value"]).as_deref(),
            Some("the token <withheld> was refused")
        );
        assert_eq!(sanitize(" \n\t ", &[]), None);
        let long = "x".repeat(MAX_MESSAGE_CHARS * 2);
        let cut = sanitize(&long, &[]).unwrap();
        assert_eq!(cut.chars().count(), MAX_MESSAGE_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_query_and_an_answer_round_trip_and_garbage_is_refused() {
        let query = HealthQuery {
            correlation_id: "round-7".into(),
        };
        assert_eq!(decode_query(&encode_query(&query)).unwrap(), query);
        let answer = HealthAnswer {
            correlation_id: "round-7".into(),
            report: HealthReport {
                status: ReportStatus::Unhealthy,
                phase: crate::HealthPhase::Normal,
                reason: Some(HealthReason::Configuration),
                message: Some("JIRA_URL is the example placeholder".into()),
                cassini_connected: true,
                graph_connected: false,
                expects_graph: false,
                counters: HealthCounters::default(),
                uptime_secs: 42,
            },
        };
        assert_eq!(decode_answer(&encode_answer(&answer)).unwrap(), answer);
        assert!(decode_query(b"no").is_err());
        assert!(decode_answer(b"").is_err());
    }

    /// A secret: printable, without whitespace, long enough to be looked for.
    fn secret() -> impl Strategy<Value = String> {
        "[A-Za-z0-9!#$%&*+./=?^_~-]{4,24}"
    }

    /// A message made of ordinary words, secrets, and the shapes credentials take.
    fn message(secrets: Vec<String>) -> impl Strategy<Value = (String, Vec<String>)> {
        let n = secrets.len();
        let piece = prop_oneof![
            "\\PC{0,30}",
            "[ \n\t\r]{1,3}",
            (0..n.max(1)).prop_map(|i| format!("#{i}")),
            "[a-z0-9]{4,12}".prop_map(|v| format!("password={v}")),
            "[a-z0-9]{4,12}".prop_map(|v| format!("JIRA_TOKEN: {v}")),
            "[a-z0-9]{4,12}".prop_map(|v| format!("Bearer {v}")),
            ("[a-z]{1,6}", "[a-z0-9]{4,12}").prop_map(|(u, p)| format!("https://{u}:{p}@host/x")),
            Just("-----BEGIN CERTIFICATE-----\nMIIB".to_string()),
        ];
        proptest::collection::vec(piece, 0..12).prop_map(move |pieces| {
            let text = pieces
                .into_iter()
                .map(
                    |p| match p.strip_prefix('#').and_then(|i| i.parse::<usize>().ok()) {
                        Some(i) if i < secrets.len() => secrets[i].clone(),
                        _ => p,
                    },
                )
                .collect::<String>();
            (text, secrets.clone())
        })
    }

    /// The credential values a message was given in a credential's shape.
    fn shaped_values(text: &str) -> Vec<String> {
        let mut values = Vec::new();
        for prefix in ["password=", "JIRA_TOKEN: ", "Bearer "] {
            for (at, _) in text.match_indices(prefix) {
                let rest = &text[at + prefix.len()..];
                let v: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    .collect();
                if v.len() >= 4 {
                    values.push(v);
                }
            }
        }
        values
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// A sanitized message is none or one line of at most the bound, and holds no secret it was given, no PEM
        /// block, no user information in a URL, and no value written in a credential's shape.
        #[test]
        fn a_sanitized_message_is_bounded_and_carries_no_secret(
            (text, secrets) in proptest::collection::vec(secret(), 0..4).prop_flat_map(message)
        ) {
            let given: Vec<&str> = secrets.iter().map(String::as_str).collect();
            let shaped = shaped_values(&text);
            if let Some(out) = sanitize(&text, &given) {
                prop_assert!(out.chars().count() <= MAX_MESSAGE_CHARS);
                prop_assert!(!out.chars().any(|c| c.is_control() || c == '\n'));
                prop_assert!(!out.is_empty() && out.trim() == out);
                for s in &secrets {
                    prop_assert!(!out.contains(s.as_str()), "{s:?} in {out:?}");
                }
                prop_assert!(!out.contains("-----BEGIN"));
                for (at, _) in out.match_indices("://") {
                    let authority: String = out[at + 3..]
                        .chars()
                        .take_while(|c| *c != '/' && !c.is_whitespace())
                        .collect();
                    if let Some(user_end) = authority.rfind('@') {
                        prop_assert!(authority[..user_end].ends_with(WITHHELD) || authority[..user_end].is_empty(),
                            "{authority:?} in {out:?}");
                    }
                }
                for v in &shaped {
                    // A value that is only a word the message has elsewhere is no evidence of a leak.
                    let elsewhere = text.replace(&format!("password={v}"), "")
                        .replace(&format!("JIRA_TOKEN: {v}"), "")
                        .replace(&format!("Bearer {v}"), "");
                    if !elsewhere.contains(v.as_str()) {
                        prop_assert!(!out.contains(&format!("={v}")) && !out.contains(&format!(": {v}"))
                            && !out.contains(&format!("Bearer {v}")), "{v:?} in {out:?}");
                    }
                }
            }
        }

        /// Sanitizing what was sanitized changes nothing.
        #[test]
        fn sanitizing_twice_is_sanitizing_once(text in "\\PC{0,300}") {
            if let Some(once) = sanitize(&text, &[]) {
                let twice = sanitize(&once, &[]);
                prop_assert_eq!(twice.as_deref(), Some(once.as_str()));
            }
        }

        /// A query and an answer read back as they were sent, and bytes cut short never panic the reader.
        #[test]
        fn the_wire_round_trips(id in "\\PC{0,24}", uptime in any::<u64>(), keep in 0usize..64) {
            let query = HealthQuery { correlation_id: id.clone() };
            let bytes = encode_query(&query);
            prop_assert_eq!(decode_query(&bytes).unwrap(), query);
            let _ = decode_query(&bytes[..keep.min(bytes.len())]);
            let answer = HealthAnswer {
                correlation_id: id,
                report: HealthReport {
                    status: ReportStatus::Healthy,
                    phase: crate::HealthPhase::Jittering,
                    reason: None,
                    message: None,
                    cassini_connected: true,
                    graph_connected: true,
                    expects_graph: true,
                    counters: HealthCounters::default(),
                    uptime_secs: uptime,
                },
            };
            prop_assert_eq!(decode_answer(&encode_answer(&answer)).unwrap(), answer);
        }
    }
}
