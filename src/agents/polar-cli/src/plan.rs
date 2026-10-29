//! A plan: what to put on a cluster, in what order, and what to wait for.
//!
//! The shape is `src/deploy/types/plan.dhall`'s, rendered to YAML. This module reads it and judges it
//! without a cluster: [`Plan::problems`] names everything wrong with a plan at once, in words about
//! the plan, so it is fixed in one pass rather than one failure at a time.

use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// How long a wait lasts unless it says otherwise.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub name: String,
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Stage {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Runs only when asked for (`--with <stage>`).
    #[serde(default)]
    pub optional: bool,
    /// First install the PolarAgent CustomResourceDefinition, which the tool carries.
    #[serde(default)]
    pub install_crd: bool,
    #[serde(default)]
    pub secrets: Vec<Secret>,
    #[serde(default)]
    pub objects: Vec<Object>,
    #[serde(default)]
    pub waits: Vec<Wait>,
}

/// A Kubernetes object, as the plan wrote it. Kept as JSON: the tool applies what it is given and
/// does not need to know every kind.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct Object(pub Json);

impl Object {
    fn text(&self, path: &[&str]) -> Option<&str> {
        let mut at = &self.0;
        for key in path {
            at = at.get(*key)?;
        }
        at.as_str().filter(|text| !text.is_empty())
    }

    pub fn api_version(&self) -> Option<&str> {
        self.text(&["apiVersion"])
    }

    pub fn kind(&self) -> Option<&str> {
        self.text(&["kind"])
    }

    pub fn name(&self) -> Option<&str> {
        self.text(&["metadata", "name"])
    }

    pub fn namespace(&self) -> Option<&str> {
        self.text(&["metadata", "namespace"])
    }

    /// Who this is, if it says enough to be applied.
    pub fn id(&self) -> Option<Id> {
        Some(Id {
            api_version: self.api_version()?.to_string(),
            kind: self.kind()?.to_string(),
            namespace: self.namespace().map(str::to_string),
            name: self.name()?.to_string(),
        })
    }
}

/// An object's identity on a cluster.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id {
    pub api_version: String,
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(namespace) => write!(f, "{} {namespace}/{}", self.kind, self.name),
            None => write!(f, "{} {}", self.kind, self.name),
        }
    }
}

/// A Secret to make if it is absent.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub namespace: String,
    pub name: String,
    /// Secrets in one group are made together, sharing the random values they name, or not at all.
    #[serde(default)]
    pub group: Option<String>,
    pub keys: BTreeMap<String, Value>,
}

/// Where a key gets its value. Told apart by field name: the Dhall union renders untagged.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Literal(Literal),
    Random(Random),
    Template(Template),
    Copy(Copy),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Literal {
    pub literal: String,
}

/// Random bytes, as lower-case hex, optionally named so that a template can use the same value.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Random {
    pub random: usize,
    #[serde(default)]
    pub id: Option<String>,
}

/// Text with `{id}` replaced by the random value of that name.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub template: String,
}

/// The value of a key of a Secret that already exists.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Copy {
    pub copy: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub namespace: String,
    pub name: String,
    pub key: String,
}

// A literal is written in the plan, but a plan is read aloud in reviews and logs: show the shape of a
// value, never the text of one.
impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Literal(_) => write!(f, "literal"),
            Value::Random(r) => write!(f, "random({} bytes)", r.random),
            Value::Template(_) => write!(f, "template"),
            Value::Copy(c) => write!(f, "copy({}/{}:{})", c.copy.namespace, c.copy.name, c.copy.key),
        }
    }
}

/// Wait until a field of an object has a value.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Wait {
    pub api_version: String,
    pub kind: String,
    #[serde(default)]
    pub namespace: Option<String>,
    pub name: String,
    /// Dotted path, for example `status.phase`; a number selects from a list.
    pub field: String,
    pub equals: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

impl Wait {
    pub fn target(&self) -> Id {
        Id {
            api_version: self.api_version.clone(),
            kind: self.kind.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
        }
    }
}

/// The ids `{name}` in a template refers to, in order, or the first malformed brace.
pub fn placeholders(template: &str) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find(['{', '}']) {
        if rest.as_bytes()[open] == b'}' {
            return Err(format!("a '}}' with no '{{' before it in \"{template}\""));
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(format!("a '{{' that is never closed in \"{template}\""));
        };
        let id = &after[..close];
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!("\"{{{id}}}\" is not an id (letters, digits, - and _) in \"{template}\""));
        }
        found.push(id.to_string());
        rest = &after[close + 1..];
    }
    Ok(found)
}

impl Plan {
    pub fn from_yaml(text: &str) -> anyhow::Result<Plan> {
        serde_yaml::from_str(text).map_err(|e| anyhow::anyhow!("not a plan: {e}"))
    }

    /// Everything wrong with the plan, judged without a cluster. Empty means it can be run.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.name.trim().is_empty() {
            problems.push("the plan has no name".to_string());
        }
        if self.stages.is_empty() {
            problems.push("the plan has no stages".to_string());
        }

        let mut stage_names = BTreeSet::new();
        // What exists once each stage has run, for judging waits.
        let mut created: BTreeSet<Id> = BTreeSet::new();
        let mut secret_names: BTreeSet<(String, String)> = BTreeSet::new();
        let mut group_stage: BTreeMap<String, String> = BTreeMap::new();

        for stage in &self.stages {
            let at = format!("stage '{}'", stage.name);
            if stage.name.trim().is_empty() {
                problems.push("a stage has no name".to_string());
            } else if !stage_names.insert(stage.name.clone()) {
                problems.push(format!("two stages are named '{}'", stage.name));
            }
            if stage.install_crd && stage.optional {
                problems.push(format!("{at} installs the CRD and is optional: stages that need it would fail without it"));
            }

            for object in &stage.objects {
                match object.id() {
                    Some(id) => {
                        if !created.insert(id.clone()) {
                            problems.push(format!("{at}: {id} is in the plan twice"));
                        }
                    }
                    None => problems.push(format!(
                        "{at}: an object has no apiVersion, kind or metadata.name ({})",
                        brief(&object.0)
                    )),
                }
            }

            // Which ids each group defines, and which secrets are in it.
            let mut group_ids: BTreeMap<Option<String>, BTreeSet<String>> = BTreeMap::new();
            for secret in &stage.secrets {
                let name = format!("{}/{}", secret.namespace, secret.name);
                if secret.namespace.trim().is_empty() || secret.name.trim().is_empty() {
                    problems.push(format!("{at}: a secret needs a namespace and a name"));
                } else if !secret_names.insert((secret.namespace.clone(), secret.name.clone())) {
                    problems.push(format!("{at}: secret {name} is in the plan twice"));
                }
                if secret.keys.is_empty() {
                    problems.push(format!("{at}: secret {name} has no keys"));
                }
                if let Some(group) = &secret.group {
                    match group_stage.get(group) {
                        Some(first) if first != &stage.name => problems.push(format!(
                            "{at}: group '{group}' is also in stage '{first}'; a group is made together, so it belongs to one stage"
                        )),
                        _ => {
                            group_stage.insert(group.clone(), stage.name.clone());
                        }
                    }
                }
                let ids = group_ids.entry(secret.group.clone()).or_default();
                for (key, value) in &secret.keys {
                    if let Value::Random(random) = value {
                        if random.random == 0 || random.random > 1024 {
                            problems.push(format!("{at}: {name} key '{key}' asks for {} random bytes; 1 to 1024", random.random));
                        }
                        if let Some(id) = &random.id
                            && !ids.insert(id.clone())
                        {
                            problems.push(format!("{at}: the id '{id}' is defined twice in {}", scope(&secret.group)));
                        }
                    }
                    if let Value::Copy(copy) = value
                        && copy.copy.namespace == secret.namespace
                        && copy.copy.name == secret.name
                    {
                        problems.push(format!("{at}: {name} key '{key}' copies from itself"));
                    }
                }
            }
            for secret in &stage.secrets {
                let name = format!("{}/{}", secret.namespace, secret.name);
                let defined = group_ids.get(&secret.group).cloned().unwrap_or_default();
                for (key, value) in &secret.keys {
                    if let Value::Template(template) = value {
                        match placeholders(&template.template) {
                            Err(why) => problems.push(format!("{at}: {name} key '{key}': {why}")),
                            Ok(ids) => {
                                for id in ids.iter().filter(|id| !defined.contains(*id)) {
                                    problems.push(format!(
                                        "{at}: {name} key '{key}' uses {{{id}}}, which no random value in {} is named",
                                        scope(&secret.group)
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            for wait in &stage.waits {
                let target = wait.target();
                if wait.api_version.is_empty() || wait.kind.is_empty() || wait.name.is_empty() {
                    problems.push(format!("{at}: a wait needs an apiVersion, a kind and a name"));
                    continue;
                }
                if wait.field.split('.').any(str::is_empty) {
                    problems.push(format!("{at}: wait on {target}: '{}' is not a dotted path", wait.field));
                }
                if wait.timeout_secs == 0 {
                    problems.push(format!("{at}: wait on {target} has a timeout of zero"));
                }
                if !created.contains(&target) {
                    problems.push(format!(
                        "{at}: waits on {target}, which is not an object of this stage or an earlier one"
                    ));
                }
            }
        }
        problems
    }

    /// The stages to run: every stage that is not optional, plus the optional ones asked for with
    /// `with`; or exactly those named by `only`.
    pub fn select(&self, with: &[String], only: &[String]) -> anyhow::Result<Vec<&Stage>> {
        if !with.is_empty() && !only.is_empty() {
            anyhow::bail!("--with and --only cannot be used together");
        }
        for asked in with.iter().chain(only) {
            if !self.stages.iter().any(|s| &s.name == asked) {
                anyhow::bail!(
                    "plan '{}' has no stage '{asked}' (it has: {})",
                    self.name,
                    self.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
                );
            }
        }
        for asked in with {
            if self.stages.iter().any(|s| &s.name == asked && !s.optional) {
                anyhow::bail!("stage '{asked}' is not optional; it runs without being asked for");
            }
        }
        Ok(self
            .stages
            .iter()
            .filter(|s| {
                if only.is_empty() {
                    !s.optional || with.contains(&s.name)
                } else {
                    only.contains(&s.name)
                }
            })
            .collect())
    }

    /// Every object of every stage, in order, for reading a plan as data.
    pub fn objects(&self) -> impl Iterator<Item = &Object> {
        self.stages.iter().flat_map(|s| s.objects.iter())
    }

    /// A readable account of the plan, secrets by name only.
    pub fn describe(&self) -> String {
        let mut out = format!("plan {} ({} stages)\n", self.name, self.stages.len());
        for stage in &self.stages {
            out.push_str(&format!("\n== {}", stage.name));
            if stage.optional {
                out.push_str("  [optional: --with ");
                out.push_str(&stage.name);
                out.push(']');
            }
            if let Some(description) = &stage.description {
                out.push_str(&format!("\n   {description}"));
            }
            out.push('\n');
            if stage.install_crd {
                out.push_str("   install  the PolarAgent CRD\n");
            }
            for secret in &stage.secrets {
                let group = secret.group.as_deref().map(|g| format!(" (group {g})")).unwrap_or_default();
                let keys: Vec<String> = secret.keys.iter().map(|(k, v)| format!("{k}: {v:?}")).collect();
                out.push_str(&format!("   secret   {}/{}{group}  [{}]\n", secret.namespace, secret.name, keys.join(", ")));
            }
            for object in &stage.objects {
                match object.id() {
                    Some(id) => out.push_str(&format!("   apply    {id}\n")),
                    None => out.push_str("   apply    (an object with no identity)\n"),
                }
            }
            for wait in &stage.waits {
                out.push_str(&format!(
                    "   wait     {} until {} = {}  (up to {}s)\n",
                    wait.target(),
                    wait.field,
                    wait.equals,
                    wait.timeout_secs
                ));
            }
        }
        out
    }
}

fn scope(group: &Option<String>) -> String {
    match group {
        Some(group) => format!("group '{group}'"),
        None => "the secret".to_string(),
    }
}

fn brief(json: &Json) -> String {
    let text = json.to_string();
    if text.chars().count() > 80 {
        format!("{}...", text.chars().take(80).collect::<String>())
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(yaml: &str) -> Plan {
        Plan::from_yaml(yaml).expect("a plan")
    }

    const GOOD: &str = r#"
name: demo
stages:
  - name: base
    installCrd: true
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: demo } }
      - { apiVersion: apps/v1, kind: Deployment, metadata: { name: web, namespace: demo } }
    waits:
      - { apiVersion: apps/v1, kind: Deployment, namespace: demo, name: web, field: status.readyReplicas, equals: "1" }
  - name: graph
    optional: true
    secrets:
      - { namespace: demo, name: pw, group: g, keys: { secret: { random: 16, id: pw } } }
      - { namespace: demo, name: auth, group: g, keys: { secret: { template: "neo4j/{pw}" } } }
"#;

    #[test]
    fn a_sound_plan_has_no_problems() {
        assert_eq!(plan(GOOD).problems(), Vec::<String>::new());
    }

    #[test]
    fn the_untagged_values_are_told_apart_by_their_fields() {
        let plan = plan(
            r#"
name: p
stages:
  - name: s
    secrets:
      - name: a
        namespace: n
        keys:
          lit: { literal: admin }
          rnd: { random: 8 }
          cpy: { copy: { namespace: x, name: y, key: z } }
          tpl: { template: "a-{id}" }
"#,
        );
        let keys = &plan.stages[0].secrets[0].keys;
        assert!(matches!(keys["lit"], Value::Literal(_)));
        assert!(matches!(keys["rnd"], Value::Random(_)));
        assert!(matches!(keys["cpy"], Value::Copy(_)));
        assert!(matches!(keys["tpl"], Value::Template(_)));
    }

    #[test]
    fn a_misspelled_field_is_refused_not_ignored() {
        assert!(Plan::from_yaml("name: p\nstages:\n  - name: s\n    objcts: []\n").is_err());
        assert!(Plan::from_yaml("name: p\nstages:\n  - name: s\n    secrets:\n      - { name: a, namespace: n, keys: { k: { randm: 4 } } }\n").is_err());
    }

    #[test]
    fn every_problem_is_named_at_once() {
        let bad = plan(
            r#"
name: ""
stages:
  - name: a
    installCrd: true
    optional: true
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: n } }
      - { apiVersion: v1, kind: Namespace, metadata: { name: n } }
      - { kind: Namespace }
    secrets:
      - { namespace: n, name: s, group: g, keys: { k: { template: "{missing} and {oops" } } }
      - { namespace: n, name: s, keys: {} }
      - { namespace: n, name: t, keys: { k: { random: 0 } } }
    waits:
      - { apiVersion: v1, kind: Namespace, name: nowhere, field: "status..phase", equals: x, timeoutSecs: 0 }
  - name: b
    secrets:
      - { namespace: n, name: u, group: g, keys: { k: { copy: { namespace: n, name: u, key: k } } } }
  - name: a
"#,
        );
        let text = bad.problems().join("\n");
        for expected in [
            "has no name",
            "two stages are named 'a'",
            "installs the CRD and is optional",
            "Namespace n is in the plan twice",
            "an object has no apiVersion, kind or metadata.name",
            "secret n/s is in the plan twice",
            "n/s has no keys",
            "asks for 0 random bytes",
            "is never closed",
            "is not a dotted path",
            "a timeout of zero",
            "waits on Namespace nowhere, which is not an object",
            "copies from itself",
            "group 'g' is also in stage 'a'",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn a_template_may_only_use_ids_its_group_names() {
        let bad = plan(
            "name: p\nstages:\n  - name: s\n    secrets:\n      - { namespace: n, name: a, group: g, keys: { k: { template: 'x-{pw}' } } }\n      - { namespace: n, name: b, group: h, keys: { k: { random: 4, id: pw } } }\n",
        );
        let text = bad.problems().join("\n");
        assert!(text.contains("uses {pw}, which no random value in group 'g' is named"), "{text}");
    }

    #[test]
    fn placeholders_are_read_and_malformed_ones_refused() {
        assert_eq!(placeholders("neo4j/{pw}").unwrap(), ["pw"]);
        assert_eq!(placeholders("{a}-{b}").unwrap(), ["a", "b"]);
        assert!(placeholders("plain").unwrap().is_empty());
        assert!(placeholders("{").is_err());
        assert!(placeholders("}").is_err());
        assert!(placeholders("{}").is_err());
        assert!(placeholders("{a b}").is_err());
    }

    #[test]
    fn optional_stages_run_only_when_asked_for() {
        let plan = plan(GOOD);
        let names = |stages: Vec<&Stage>| stages.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(plan.select(&[], &[]).unwrap()), ["base"]);
        assert_eq!(names(plan.select(&["graph".into()], &[]).unwrap()), ["base", "graph"]);
        assert_eq!(names(plan.select(&[], &["graph".into()]).unwrap()), ["graph"]);
        assert!(plan.select(&["nope".into()], &[]).is_err());
        assert!(plan.select(&["base".into()], &[]).unwrap_err().to_string().contains("not optional"));
        assert!(plan.select(&["graph".into()], &["base".into()]).is_err());
    }

    #[test]
    fn a_description_names_secrets_but_never_shows_a_value() {
        let described = plan(
            "name: p\nstages:\n  - name: s\n    secrets:\n      - { namespace: n, name: a, keys: { k: { literal: hunter2-not-real } } }\n",
        )
        .describe();
        assert!(described.contains("n/a"));
        assert!(!described.contains("hunter2"), "{described}");
    }
}
