//! Running a plan.
//!
//! Within a stage the order is always the same, so a plan reads the way it runs: the CRD if asked
//! for, then the namespaces (everything else lives in one), then the secrets, then the ConfigMaps (which
//! may copy what the secrets just made), then the other objects, then the waits. The next stage starts when the waits of
//! this one have passed. `down` is the same plan backwards, and never removes the CRD.
//!
//! Written against [`Cluster`] and reporting through a callback, so the order, the waits and the
//! reports are tested with no cluster and no terminal.

use crate::cluster::{Applied, Cluster, Deleted};
use crate::plan::{self, Id, Object, Plan, Stage, Wait};
use crate::secrets::{self, Action};
use crate::{ca, configmaps};
use anyhow::{Result, bail};
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tokio::time::{Instant, sleep};

#[derive(Debug, Clone)]
pub struct Options {
    /// Optional stages to run as well.
    pub with: Vec<String>,
    /// Run exactly these stages.
    pub only: Vec<String>,
    /// Ask the cluster to judge each change without making it, and do not wait.
    pub dry_run: bool,
    /// How often a wait looks.
    pub poll: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options { with: Vec::new(), only: Vec::new(), dry_run: false, poll: Duration::from_secs(2) }
    }
}

/// The text at a dotted path, for comparing: strings as they are, numbers and booleans as they print.
/// A number in the path selects from a list.
pub fn field_value(object: &Json, path: &str) -> Option<String> {
    let mut at = object;
    for step in path.split('.') {
        at = match at {
            Json::Array(items) => items.get(step.parse::<usize>().ok()?)?,
            _ => at.get(step)?,
        };
    }
    match at {
        Json::String(text) => Some(text.clone()),
        Json::Number(number) => Some(number.to_string()),
        Json::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// What the operator recorded about a PolarAgent, in words: why it is not running.
pub fn explain_agent(agent: &Json) -> Vec<String> {
    let status = &agent["status"];
    let mut lines = Vec::new();
    if let Some(phase) = status["phase"].as_str() {
        match status["message"].as_str().filter(|m| !m.is_empty()) {
            Some(message) => lines.push(format!("the operator reports phase {phase}: {message}")),
            None => lines.push(format!("the operator reports phase {phase}")),
        }
    }
    let started = status["instancesStarted"].as_u64().unwrap_or(0);
    let failures = status["consecutiveFailures"].as_u64().unwrap_or(0);
    if started > 0 {
        lines.push(format!(
            "{started} instance(s) started; {failures} ending(s) in a row soon after starting"
        ));
    }
    let ending = &status["lastEnding"];
    if ending.is_object() {
        let kind = ending["kind"].as_str().unwrap_or("ended");
        let exit = ending["exitCode"].as_i64().map(|c| format!(", exit {c}")).unwrap_or_default();
        let ran = ending["ranSecs"].as_i64().map(|s| format!(" after {s}s")).unwrap_or_default();
        lines.push(format!("last ending: {kind}{exit}{ran}"));
        if let Some(message) = ending["message"].as_str().filter(|m| !m.is_empty()) {
            lines.push(format!("the instance said: {}", message.trim()));
        }
    }
    lines
}

fn say(out: &mut dyn FnMut(String), line: impl Into<String>) {
    out(line.into());
}

/// Run one plan. See [`up_all`].
pub async fn up<C: Cluster>(
    cluster: &C,
    plan: &Plan,
    options: &Options,
    random: &mut dyn FnMut(usize) -> Vec<u8>,
    out: &mut dyn FnMut(String),
) -> Result<()> {
    up_all(cluster, std::slice::from_ref(plan), options, random, out).await
}

/// Judge every plan before touching anything, then run them in order.
///
/// A stage that is `shared` (the core every application comes with) is applied once however many of
/// the plans have it; the same name with different content in two plans is an error that names both,
/// found before anything has been changed.
pub async fn up_all<C: Cluster>(
    cluster: &C,
    plans: &[Plan],
    options: &Options,
    random: &mut dyn FnMut(usize) -> Vec<u8>,
    out: &mut dyn FnMut(String),
) -> Result<()> {
    for plan in plans {
        let problems = plan.problems();
        if !problems.is_empty() {
            bail!("plan '{}' cannot be run:\n  - {}", plan.name, problems.join("\n  - "));
        }
    }
    let selected: Vec<Vec<&Stage>> =
        plans.iter().map(|p| p.select_among(&options.with, &options.only)).collect::<Result<_>>()?;

    // Shared stages by name: which plan has them first, and what they hold.
    let mut shared: BTreeMap<&str, (&str, &Stage)> = BTreeMap::new();
    for (plan, stages) in plans.iter().zip(&selected) {
        for stage in stages.iter().filter(|s| s.shared) {
            match shared.get(stage.name.as_str()) {
                Some((first, held)) if *held != *stage => bail!(
                    "the shared stage '{}' is in plan '{}' and in plan '{}' with different contents; \
                     a shared stage is one thing, so the plans must agree on it",
                    stage.name,
                    first,
                    plan.name
                ),
                Some(_) => {}
                None => {
                    shared.insert(&stage.name, (&plan.name, stage));
                }
            }
        }
    }

    // What the plans create, so that a dry run, which creates nothing, can tell something that is missing
    // because the plan has not got there from something that is missing by mistake.
    let mut planned = Planned::default();
    let mut applied: BTreeMap<&str, &str> = BTreeMap::new();
    for (plan, stages) in plans.iter().zip(&selected) {
        say(out, format!("plan {}{}", plan.name, if options.dry_run { " (dry run: nothing is changed)" } else { "" }));
        for stage in stages {
            if stage.shared {
                if let Some(first) = applied.get(stage.name.as_str()) {
                    say(out, format!("\n== {} - already applied by plan {first}", stage.name));
                    continue;
                }
                applied.insert(&stage.name, &plan.name);
            }
            run_stage(cluster, stage, options, random, out, &mut planned).await?;
        }
    }
    Ok(())
}

fn is_namespace(object: &Object) -> bool {
    object.api_version() == Some("v1") && object.kind() == Some("Namespace")
}

/// What a run has said it will create, in a dry run where nothing is created.
#[derive(Default)]
struct Planned {
    namespaces: BTreeSet<String>,
    secrets: BTreeSet<(String, String)>,
}

/// In a dry run: is this namespace one the plan creates and the cluster does not have yet? Then
/// nothing in it can be judged by the cluster, and the tool says so instead of failing.
async fn pending<C: Cluster>(cluster: &C, namespace: &str, planned: &Planned) -> Result<bool> {
    if !planned.namespaces.contains(namespace) {
        return Ok(false);
    }
    let id = Id {
        api_version: "v1".to_string(),
        kind: "Namespace".to_string(),
        namespace: None,
        name: namespace.to_string(),
    };
    Ok(cluster.get(&id).await?.is_none())
}

async fn run_stage<C: Cluster>(
    cluster: &C,
    stage: &Stage,
    options: &Options,
    random: &mut dyn FnMut(usize) -> Vec<u8>,
    out: &mut dyn FnMut(String),
    planned: &mut Planned,
) -> Result<()> {
    let dry = options.dry_run;
    say(out, format!("\n== {}{}", stage.name, stage.description.as_ref().map(|d| format!(" - {d}")).unwrap_or_default()));

    if stage.install_crd {
        cluster.install_crd(dry).await?;
        say(out, if dry { "   would install the PolarAgent CRD" } else { "   installed the PolarAgent CRD" });
    }

    // Namespaces first: the secrets and most objects of the stage live in them.
    for object in stage.objects.iter().filter(|o| is_namespace(o)) {
        if let Some(name) = object.name() {
            planned.namespaces.insert(name.to_string());
        }
        apply_one(cluster, object, dry, None, out).await?;
    }

    if !stage.secrets.is_empty() {
        let mut present = BTreeSet::new();
        for secret in &stage.secrets {
            if cluster.secret(&secret.namespace, &secret.name).await?.is_some() {
                present.insert((secret.namespace.clone(), secret.name.clone()));
            }
        }
        let mut copies = BTreeMap::new();
        for source in secrets::copy_sources(&stage.secrets) {
            if let Some(held) = cluster.secret(&source.namespace, &source.name).await?
                && let Some(value) = held.get(&source.key)
            {
                copies.insert(source, value.clone());
            }
        }
        let mut authority = |spec: &plan::CaSpec| ca::generate(&spec.common_name, spec.validity_days);
        for action in secrets::decide(&stage.secrets, &present, &copies, random, &mut authority)? {
            match action {
                Action::Exists { .. } => say(out, format!("   secret {} exists", action.name())),
                Action::Create { namespace, name, data } => {
                    let keys = data.keys().cloned().collect::<Vec<_>>().join(", ");
                    planned.secrets.insert((namespace.clone(), name.clone()));
                    if dry && pending(cluster, &namespace, planned).await? {
                        say(out, format!("   secret {namespace}/{name} would be created ({keys}); its namespace is created by this plan"));
                        continue;
                    }
                    cluster.create_secret(&namespace, &name, &data, dry).await?;
                    say(out, format!("   secret {namespace}/{name} {} ({keys})", if dry { "would be created" } else { "created" }));
                }
            }
        }
    }

    if !stage.config_maps.is_empty() {
        let mut held = BTreeMap::new();
        for config_map in &stage.config_maps {
            if let Some(data) = cluster.config_map(&config_map.namespace, &config_map.name).await? {
                held.insert((config_map.namespace.clone(), config_map.name.clone()), data);
            }
        }
        // The Secrets are read after the stage's own were made, so a ConfigMap can copy what they hold.
        let mut copies = BTreeMap::new();
        for source in configmaps::copy_sources(&stage.config_maps) {
            if let Some(found) = cluster.secret(&source.namespace, &source.name).await?
                && let Some(value) = found.get(&source.key)
            {
                copies.insert(source, value.clone());
            }
        }
        for action in configmaps::decide(&stage.config_maps, &held, &copies)? {
            match action {
                configmaps::Action::Unchanged { .. } => say(out, format!("   configmap {} is as it should be", action.name())),
                configmaps::Action::Blocked { namespace, name, source } => {
                    let made_by_the_plan = planned.secrets.contains(&(source.namespace.clone(), source.name.clone()));
                    if dry && made_by_the_plan {
                        say(out, format!("   configmap {namespace}/{name} would be written once secret {}/{} is made", source.namespace, source.name));
                        continue;
                    }
                    bail!(
                        "configmap {namespace}/{name} copies key '{}' of secret {}/{}, which is not there",
                        source.key,
                        source.namespace,
                        source.name
                    );
                }
                configmaps::Action::Write { namespace, name, existed, data } => {
                    let keys = data.keys().cloned().collect::<Vec<_>>().join(", ");
                    if dry && pending(cluster, &namespace, planned).await? {
                        say(out, format!("   configmap {namespace}/{name} would be created ({keys}); its namespace is created by this plan"));
                        continue;
                    }
                    cluster.write_config_map(&namespace, &name, &data, dry).await?;
                    let did = match (dry, existed) {
                        (true, false) => "would be created",
                        (true, true) => "would be updated",
                        (false, false) => "created",
                        (false, true) => "updated to match its source",
                    };
                    say(out, format!("   configmap {namespace}/{name} {did} ({keys})"));
                }
            }
        }
    }

    for object in stage.objects.iter().filter(|o| !is_namespace(o)) {
        let namespace = if dry { object.namespace() } else { None };
        let held_back = match namespace {
            Some(ns) => pending(cluster, ns, planned).await?,
            None => false,
        };
        apply_one(cluster, object, dry, held_back.then_some("its namespace is created by this plan"), out).await?;
    }

    for wait in &stage.waits {
        if dry {
            say(out, format!("   would wait for {} until {} = {}", wait.target(), wait.field, wait.equals));
        } else {
            wait_for(cluster, wait, options.poll, out).await?;
        }
    }
    Ok(())
}

/// Apply one object and say what came of it. `hold_back` is why a dry run cannot ask the cluster.
async fn apply_one<C: Cluster>(
    cluster: &C,
    object: &Object,
    dry: bool,
    hold_back: Option<&str>,
    out: &mut dyn FnMut(String),
) -> Result<()> {
    let id = object.id().expect("a validated plan's objects have identities");
    if let Some(why) = hold_back {
        say(out, format!("   would apply {id} ({why}; a dry run cannot judge it)"));
        return Ok(());
    }
    match cluster.apply(object, dry).await? {
        Applied::Done => say(out, format!("   {} {id}", if dry { "would apply" } else { "applied" })),
        Applied::UnknownKind => say(
            out,
            format!("   would apply {id} (its kind is installed by an earlier stage; a dry run cannot see it)"),
        ),
    }
    Ok(())
}

async fn wait_for<C: Cluster>(
    cluster: &C,
    wait: &Wait,
    poll: Duration,
    out: &mut dyn FnMut(String),
) -> Result<()> {
    let id = wait.target();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(wait.timeout_secs);
    say(out, format!("   waiting for {id}: {} = {}", wait.field, wait.equals));
    loop {
        let current = cluster.get(&id).await?;
        if let Some(object) = &current
            && field_value(object, &wait.field).as_deref() == Some(wait.equals.as_str())
        {
            say(out, format!("   ready: {id} ({}s)", started.elapsed().as_secs()));
            return Ok(());
        }
        if Instant::now() >= deadline {
            let mut message = format!(
                "timed out after {}s waiting for {id} to have {} = {}",
                wait.timeout_secs, wait.field, wait.equals
            );
            match &current {
                None => message.push_str("\n  it does not exist on the cluster"),
                Some(object) => {
                    let has = field_value(object, &wait.field).unwrap_or_else(|| "nothing".to_string());
                    message.push_str(&format!("\n  it has {} = {has}", wait.field));
                    if id.kind == "PolarAgent" {
                        for line in explain_agent(object) {
                            message.push_str(&format!("\n  {line}"));
                        }
                    }
                }
            }
            bail!("{message}");
        }
        sleep(poll).await;
    }
}

/// The plan backwards: each stage's objects, then its ConfigMaps, then its secrets, last stage first. The CRD stays.
///
/// A shared stage belongs to every plan that includes it, so it stays unless `core` is asked for.
pub async fn down<C: Cluster>(cluster: &C, plan: &Plan, core: bool, out: &mut dyn FnMut(String)) -> Result<()> {
    say(out, format!("plan {} (taking it down)", plan.name));
    for stage in plan.stages.iter().rev() {
        if stage.shared && !core {
            say(out, format!("\n== {} - shared, left in place (--core takes it down too)", stage.name));
            continue;
        }
        say(out, format!("\n== {}", stage.name));
        for object in stage.objects.iter().rev() {
            let Some(id) = object.id() else { continue };
            report_delete(out, &id.to_string(), cluster.delete(&id).await?);
        }
        for config_map in stage.config_maps.iter().rev() {
            let name = format!("configmap {}/{}", config_map.namespace, config_map.name);
            report_delete(out, &name, cluster.delete_config_map(&config_map.namespace, &config_map.name).await?);
        }
        for secret in stage.secrets.iter().rev() {
            let name = format!("secret {}/{}", secret.namespace, secret.name);
            report_delete(out, &name, cluster.delete_secret(&secret.namespace, &secret.name).await?);
        }
    }
    say(out, "\nthe PolarAgent CRD is left in place: it may be shared, and removing it deletes every PolarAgent");
    Ok(())
}

fn report_delete(out: &mut dyn FnMut(String), what: &str, result: Deleted) {
    match result {
        Deleted::Removed => say(out, format!("   deleted {what}")),
        Deleted::Absent => say(out, format!("   {what} was not there")),
    }
}

/// For callers that want the identity of a wait's target without building a plan.
pub fn target(wait: &Wait) -> Id {
    wait.target()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::fake::Fake;
    use crate::plan::Plan;
    use serde_json::json;

    const PLAN: &str = r#"
name: demo
stages:
  - name: operator
    installCrd: true
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: ops } }
      - { apiVersion: apps/v1, kind: Deployment, metadata: { name: operator, namespace: ops } }
    waits:
      - { apiVersion: apps/v1, kind: Deployment, namespace: ops, name: operator, field: status.readyReplicas, equals: "1", timeoutSecs: 30 }
  - name: platform
    secrets:
      - { namespace: demo, name: token, keys: { token: { random: 4 } } }
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: demo } }
      - { apiVersion: polar.sei.cmu.edu/v1alpha1, kind: PolarAgent, metadata: { name: cassini, namespace: demo } }
    waits:
      - { apiVersion: polar.sei.cmu.edu/v1alpha1, kind: PolarAgent, namespace: demo, name: cassini, field: status.phase, equals: Running, timeoutSecs: 20 }
  - name: graph
    optional: true
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: graph } }
"#;

    fn counting() -> impl FnMut(usize) -> Vec<u8> {
        let mut next = 0u8;
        move |n| (0..n).map(|_| { next += 1; next }).collect()
    }

    fn ready() -> Box<dyn Fn(&Id, usize) -> Option<Json>> {
        Box::new(|id, _| {
            Some(match id.kind.as_str() {
                "Deployment" => json!({ "status": { "readyReplicas": 1 } }),
                _ => json!({ "status": { "phase": "Running" } }),
            })
        })
    }

    async fn run(cluster: &Fake, options: Options) -> (Result<()>, Vec<String>) {
        let plan = Plan::from_yaml(PLAN).unwrap();
        let mut lines = Vec::new();
        let result = up(cluster, &plan, &options, &mut counting(), &mut |l| lines.push(l)).await;
        (result, lines)
    }

    fn fast() -> Options {
        Options { poll: Duration::from_millis(10), ..Options::default() }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stage_runs_crd_then_secrets_then_objects_then_waits_and_the_next_waits_for_it() {
        let cluster = Fake { crd_kinds: vec!["PolarAgent".into()], ..Fake::default() }.with_script(ready());
        let (result, _) = run(&cluster, fast()).await;
        result.unwrap();
        assert_eq!(
            cluster.calls(),
            [
                "crd",
                "apply Namespace ops",
                "apply Deployment ops/operator",
                "get Deployment ops/operator",
                "apply Namespace demo",
                "secret demo/token [token]",
                "apply PolarAgent demo/cassini",
                "get PolarAgent demo/cassini",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stages_namespaces_come_before_its_secrets_whatever_order_the_plan_lists_them() {
        // The secret's namespace is an object of the same stage, listed after everything else.
        let plan = Plan::from_yaml(
            r#"
name: p
stages:
  - name: s
    secrets:
      - { namespace: late, name: pw, keys: { k: { random: 2 } } }
    objects:
      - { apiVersion: apps/v1, kind: Deployment, metadata: { name: web, namespace: late } }
      - { apiVersion: v1, kind: Namespace, metadata: { name: late } }
"#,
        )
        .unwrap();
        let cluster = Fake::default();
        up(&cluster, &plan, &fast(), &mut counting(), &mut |_| {}).await.unwrap();
        assert_eq!(cluster.calls(), ["apply Namespace late", "secret late/pw [k]", "apply Deployment late/web"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dry_run_does_not_fail_on_a_namespace_the_plan_has_yet_to_create() {
        let plan = Plan::from_yaml(
            r#"
name: p
stages:
  - name: s
    secrets:
      - { namespace: later, name: pw, keys: { k: { random: 2 } } }
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: later } }
      - { apiVersion: apps/v1, kind: Deployment, metadata: { name: web, namespace: later } }
      - { apiVersion: apps/v1, kind: Deployment, metadata: { name: other, namespace: elsewhere } }
"#,
        )
        .unwrap();
        let cluster = Fake::default();
        let mut lines = Vec::new();
        up(&cluster, &plan, &Options { dry_run: true, ..fast() }, &mut counting(), &mut |l| lines.push(l)).await.unwrap();
        let said = lines.join("\n");
        assert!(said.contains("secret later/pw would be created (k); its namespace is created by this plan"), "{said}");
        assert!(said.contains("would apply Deployment later/web (its namespace is created by this plan"), "{said}");
        // A namespace the plan does not create is the cluster's to judge, as before.
        let calls = cluster.calls();
        assert!(calls.contains(&"apply Deployment elsewhere/other (dry run)".to_string()), "{calls:?}");
        assert!(!calls.iter().any(|c| c.starts_with("secret ") || c.contains("later/web")), "{calls:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_optional_stage_runs_only_when_asked_for() {
        let cluster = Fake::default().with_script(ready());
        run(&cluster, fast()).await.0.unwrap();
        assert!(!cluster.calls().iter().any(|c| c.contains("Namespace graph")));

        let cluster = Fake::default().with_script(ready());
        run(&cluster, Options { with: vec!["graph".into()], ..fast() }).await.0.unwrap();
        assert!(cluster.calls().iter().any(|c| c == "apply Namespace graph"));
    }

    #[tokio::test(start_paused = true)]
    async fn running_it_again_makes_no_second_secret() {
        let cluster = Fake::default().with_script(ready());
        run(&cluster, fast()).await.0.unwrap();
        let (result, lines) = run(&cluster, fast()).await;
        result.unwrap();
        assert_eq!(cluster.calls().iter().filter(|c| c.starts_with("secret demo/token")).count(), 1);
        assert!(lines.iter().any(|l| l.contains("secret demo/token exists")), "{lines:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_looks_again_until_the_field_has_its_value() {
        let cluster = Fake::default().with_script(Box::new(|id, n| {
            Some(match id.kind.as_str() {
                "Deployment" if n < 3 => json!({ "status": { "readyReplicas": 0 } }),
                "Deployment" => json!({ "status": { "readyReplicas": 1 } }),
                _ => json!({ "status": { "phase": "Running" } }),
            })
        }));
        run(&cluster, fast()).await.0.unwrap();
        let looks = cluster.calls().iter().filter(|c| *c == "get Deployment ops/operator").count();
        assert_eq!(looks, 4, "three looks that were not ready, then one that was");
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_that_times_out_says_what_the_operator_recorded() {
        let cluster = Fake::default().with_script(Box::new(|id, _| {
            Some(match id.kind.as_str() {
                "Deployment" => json!({ "status": { "readyReplicas": 1 } }),
                _ => json!({ "status": {
                    "phase": "BackingOff",
                    "message": "waiting out the backoff",
                    "instancesStarted": 3,
                    "consecutiveFailures": 3,
                    "lastEnding": { "kind": "Exited", "exitCode": 1, "ranSecs": 4, "message": "no certificate: the issuer refused the token" }
                } }),
            })
        }));
        let (result, _) = run(&cluster, fast()).await;
        let error = result.unwrap_err().to_string();
        for expected in [
            "timed out after 20s waiting for PolarAgent demo/cassini",
            "it has status.phase = BackingOff",
            "the operator reports phase BackingOff: waiting out the backoff",
            "3 instance(s) started; 3 ending(s) in a row",
            "last ending: Exited, exit 1 after 4s",
            "the instance said: no certificate: the issuer refused the token",
        ] {
            assert!(error.contains(expected), "missing {expected:?} in:\n{error}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_on_something_that_never_appears_says_so() {
        let cluster = Fake::default().with_script(Box::new(|id, _| {
            (id.kind == "Deployment").then(|| json!({ "status": { "readyReplicas": 1 } }))
        }));
        let error = run(&cluster, fast()).await.0.unwrap_err().to_string();
        assert!(error.contains("it does not exist on the cluster"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_dry_run_changes_nothing_waits_for_nothing_and_says_what_it_cannot_see() {
        let cluster = Fake { crd_kinds: vec!["PolarAgent".into()], ..Fake::default() };
        let (result, lines) = run(&cluster, Options { dry_run: true, ..fast() }).await;
        result.unwrap();
        assert!(cluster.objects.borrow().is_empty() && cluster.secrets.borrow().is_empty());
        // It may look at whether a namespace exists yet; it never looks at anything it would wait for.
        assert!(
            !cluster.calls().iter().any(|c| c.starts_with("get ") && !c.starts_with("get Namespace")),
            "a dry run waits for nothing"
        );
        let said = lines.join("\n");
        assert!(said.contains("would apply Namespace ops"), "{said}");
        assert!(said.contains("would apply PolarAgent demo/cassini (its namespace is created by this plan"), "{said}");
        assert!(said.contains("would install the PolarAgent CRD"), "{said}");
        assert!(said.contains("would wait for Deployment ops/operator"), "{said}");
        assert!(said.contains("secret demo/token would be created"), "{said}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_plan_with_problems_is_refused_before_anything_is_touched() {
        let cluster = Fake::default();
        let mut plan = Plan::from_yaml(PLAN).unwrap();
        plan.stages[1].waits[0].name = "nothing".into();
        let error = up(&cluster, &plan, &fast(), &mut counting(), &mut |_| {}).await.unwrap_err().to_string();
        assert!(error.contains("cannot be run") && error.contains("nothing"), "{error}");
        assert!(cluster.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn no_secret_value_reaches_the_output() {
        let cluster = Fake::default().with_script(ready());
        let (result, lines) = run(&cluster, fast()).await;
        result.unwrap();
        let held = cluster.secrets.borrow().values().next().unwrap().values().next().unwrap().clone();
        assert_eq!(held, "01020304");
        assert!(!lines.join("\n").contains(&held));
        assert!(!cluster.calls().join("\n").contains(&held));
    }

    #[tokio::test(start_paused = true)]
    async fn down_is_the_plan_backwards_and_leaves_the_crd() {
        let cluster = Fake::default().with_script(ready());
        run(&cluster, Options { with: vec!["graph".into()], ..fast() }).await.0.unwrap();
        cluster.calls.borrow_mut().clear();

        let plan = Plan::from_yaml(PLAN).unwrap();
        let mut lines = Vec::new();
        down(&cluster, &plan, true, &mut |l| lines.push(l)).await.unwrap();
        assert_eq!(
            cluster.calls(),
            [
                "delete Namespace graph",
                "delete PolarAgent demo/cassini",
                "delete Namespace demo",
                "delete secret demo/token",
                "delete Deployment ops/operator",
                "delete Namespace ops",
            ]
        );
        assert!(lines.join("\n").contains("CRD is left in place"));
        assert!(!cluster.calls().iter().any(|c| c.contains("crd")));
    }

    const CORE: &str = r#"
name: {name}
stages:
  - name: operator
    shared: true
    installCrd: true
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: ops } }
  - name: broker
    shared: true
    secrets:
      - { namespace: core, name: token, keys: { token: { random: 4 } } }
    objects:
      - { apiVersion: v1, kind: Namespace, metadata: { name: core } }
      - { apiVersion: polar.sei.cmu.edu/v1alpha1, kind: PolarAgent, metadata: { name: cassini, namespace: core } }
  - name: app
    objects:
      - { apiVersion: polar.sei.cmu.edu/v1alpha1, kind: PolarAgent, metadata: { name: {name}-agent, namespace: core } }
"#;

    fn core_plan(name: &str) -> Plan {
        Plan::from_yaml(&CORE.replace("{name}", name)).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn two_plans_that_share_the_core_apply_it_once() {
        let cluster = Fake { crd_kinds: vec!["PolarAgent".into()], ..Fake::default() };
        let plans = [core_plan("blitz"), core_plan("ace")];
        let mut lines = Vec::new();
        up_all(&cluster, &plans, &fast(), &mut counting(), &mut |l| lines.push(l)).await.unwrap();

        let calls = cluster.calls();
        let count = |what: &str| calls.iter().filter(|c| *c == what).count();
        assert_eq!(count("crd"), 1, "the core is installed once");
        assert_eq!(count("apply PolarAgent core/cassini"), 1);
        assert_eq!(count("apply PolarAgent core/blitz-agent"), 1);
        assert_eq!(count("apply PolarAgent core/ace-agent"), 1, "each application's own stage runs");
        let said = lines.join("\n");
        assert!(said.contains("== operator - already applied by plan blitz"), "{said}");
        assert!(said.contains("== broker - already applied by plan blitz"), "{said}");
    }

    #[tokio::test(start_paused = true)]
    async fn plans_that_disagree_about_a_shared_stage_are_refused_before_anything_is_touched() {
        let cluster = Fake::default();
        let mut other = core_plan("ace");
        other.stages[1].secrets[0].keys.clear();
        other.stages[1].secrets[0].keys.insert(
            "token".into(),
            crate::plan::Plan::from_yaml("name: x\nstages:\n  - name: s\n    secrets:\n      - { namespace: n, name: a, keys: { token: { random: 9 } } }\n")
                .unwrap()
                .stages
                .remove(0)
                .secrets
                .remove(0)
                .keys
                .remove("token")
                .unwrap(),
        );
        let error = up_all(&cluster, &[core_plan("blitz"), other], &fast(), &mut counting(), &mut |_| {})
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("shared stage 'broker'") && error.contains("'blitz'") && error.contains("'ace'"), "{error}");
        assert!(cluster.calls().is_empty(), "{:?}", cluster.calls());
    }

    #[tokio::test(start_paused = true)]
    async fn down_leaves_the_shared_stages_unless_the_core_is_asked_for() {
        let cluster = Fake::default().with_script(ready());
        let plan = core_plan("blitz");
        up(&cluster, &plan, &fast(), &mut counting(), &mut |_| {}).await.unwrap();
        cluster.calls.borrow_mut().clear();

        let mut lines = Vec::new();
        down(&cluster, &plan, false, &mut |l| lines.push(l)).await.unwrap();
        assert_eq!(cluster.calls(), ["delete PolarAgent core/blitz-agent"], "only the application goes");
        assert!(lines.join("\n").contains("shared, left in place"));

        cluster.calls.borrow_mut().clear();
        down(&cluster, &plan, true, &mut |_| {}).await.unwrap();
        assert!(cluster.calls().iter().any(|c| c == "delete Namespace ops"), "{:?}", cluster.calls());
    }

    #[test]
    fn a_field_is_found_by_its_dotted_path_and_compared_as_text() {
        let object = json!({ "status": { "phase": "Running", "readyReplicas": 1, "ok": true, "list": [{ "name": "a" }] } });
        assert_eq!(field_value(&object, "status.phase").as_deref(), Some("Running"));
        assert_eq!(field_value(&object, "status.readyReplicas").as_deref(), Some("1"));
        assert_eq!(field_value(&object, "status.ok").as_deref(), Some("true"));
        assert_eq!(field_value(&object, "status.list.0.name").as_deref(), Some("a"));
        assert_eq!(field_value(&object, "status.missing"), None);
        assert_eq!(field_value(&object, "status"), None, "an object is not a value");
    }

    const CA_PLAN: &str = r#"
name: ca
stages:
  - name: ca
    shared: true
    secrets:
      - { namespace: polar, name: cert-issuer-ca, keys: {
            ca.crt: { ca: { id: root, part: certificate, commonName: Test CA, validityDays: 30 } },
            ca.key: { ca: { id: root, part: key, commonName: Test CA, validityDays: 30 } } } }
    configMaps:
      - { namespace: polar, name: polar-ca, keys: { ca.pem: { copy: { namespace: polar, name: cert-issuer-ca, key: ca.crt } } } }
  - name: app
    configMaps:
      - { namespace: blitz, name: polar-ca, keys: { ca.pem: { copy: { namespace: polar, name: cert-issuer-ca, key: ca.crt } } } }
"#;

    async fn run_ca(cluster: &Fake, options: Options) -> (Result<()>, Vec<String>) {
        let plan = Plan::from_yaml(CA_PLAN).unwrap();
        let mut lines = Vec::new();
        let result = up(cluster, &plan, &options, &mut counting(), &mut |l| lines.push(l)).await;
        (result, lines)
    }

    fn ca_pair(cluster: &Fake) -> (String, String) {
        let secrets = cluster.secrets.borrow();
        let held = &secrets[&("polar".to_string(), "cert-issuer-ca".to_string())];
        (held["ca.crt"].clone(), held["ca.key"].clone())
    }

    #[tokio::test(start_paused = true)]
    async fn the_cluster_gets_a_ca_and_every_namespace_the_certificate_and_only_that() {
        let cluster = Fake::default();
        let (result, lines) = run_ca(&cluster, fast()).await;
        result.unwrap();

        let (certificate, key) = ca_pair(&cluster);
        assert!(certificate.contains("BEGIN CERTIFICATE") && key.contains("PRIVATE KEY"));
        for namespace in ["polar", "blitz"] {
            let maps = cluster.config_maps.borrow();
            let held = &maps[&(namespace.to_string(), "polar-ca".to_string())];
            assert_eq!(held["ca.pem"], certificate, "{namespace} pins the certificate the issuer signs with");
            assert!(!held.values().any(|v| v.contains("PRIVATE")), "a ConfigMap is readable by every pod in its namespace");
        }
        assert_eq!(
            cluster.calls(),
            ["secret polar/cert-issuer-ca [ca.crt,ca.key]", "configmap polar/polar-ca [ca.pem]", "configmap blitz/polar-ca [ca.pem]"],
            "the secret first, then what copies it"
        );
        let said = lines.join("\n");
        assert!(!said.contains(&key) && !said.contains("PRIVATE"), "the key never reaches the output");
    }

    #[tokio::test(start_paused = true)]
    async fn running_it_again_keeps_the_ca_and_touches_nothing() {
        let cluster = Fake::default();
        run_ca(&cluster, fast()).await.0.unwrap();
        let first = ca_pair(&cluster);
        cluster.calls.borrow_mut().clear();

        let (result, lines) = run_ca(&cluster, fast()).await;
        result.unwrap();
        assert_eq!(ca_pair(&cluster), first, "a new CA would orphan every certificate already issued");
        assert!(cluster.calls().is_empty(), "{:?}", cluster.calls());
        let said = lines.join("\n");
        assert!(said.contains("secret polar/cert-issuer-ca exists") && said.contains("configmap blitz/polar-ca is as it should be"), "{said}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_configmap_that_pins_a_ca_that_is_gone_follows_the_new_one() {
        let cluster = Fake::default();
        run_ca(&cluster, fast()).await.0.unwrap();
        let old = ca_pair(&cluster).0;
        // An admin deleted the Secret on purpose, to replace the CA.
        cluster.secrets.borrow_mut().clear();

        let (result, lines) = run_ca(&cluster, fast()).await;
        result.unwrap();
        let new = ca_pair(&cluster).0;
        assert_ne!(new, old);
        let maps = cluster.config_maps.borrow();
        assert_eq!(maps[&("polar".to_string(), "polar-ca".to_string())]["ca.pem"], new);
        assert_eq!(maps[&("blitz".to_string(), "polar-ca".to_string())]["ca.pem"], new);
        assert!(lines.join("\n").contains("configmap blitz/polar-ca updated to match its source"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_dry_run_makes_no_ca_and_says_what_waits_on_it() {
        let cluster = Fake::default();
        let (result, lines) = run_ca(&cluster, Options { dry_run: true, ..fast() }).await;
        result.unwrap();
        assert!(cluster.secrets.borrow().is_empty() && cluster.config_maps.borrow().is_empty());
        let said = lines.join("\n");
        assert!(said.contains("secret polar/cert-issuer-ca would be created (ca.crt, ca.key)"), "{said}");
        assert!(said.contains("configmap polar/polar-ca would be written once secret polar/cert-issuer-ca is made"), "{said}");
        assert!(said.contains("configmap blitz/polar-ca would be written once secret polar/cert-issuer-ca is made"), "{said}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_configmap_that_copies_a_secret_nothing_makes_is_an_error_that_names_it() {
        let cluster = Fake::default();
        let plan = Plan::from_yaml(
            "name: p\nstages:\n  - name: s\n    configMaps:\n      - { namespace: n, name: c, keys: { k: { copy: { namespace: x, name: y, key: z } } } }\n",
        )
        .unwrap();
        let error = up(&cluster, &plan, &fast(), &mut counting(), &mut |_| {}).await.unwrap_err().to_string();
        assert!(error.contains("configmap n/c copies key 'z' of secret x/y, which is not there"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn down_removes_configmaps_before_the_secrets_they_copy() {
        let cluster = Fake::default();
        run_ca(&cluster, fast()).await.0.unwrap();
        cluster.calls.borrow_mut().clear();

        let plan = Plan::from_yaml(CA_PLAN).unwrap();
        down(&cluster, &plan, true, &mut |_| {}).await.unwrap();
        assert_eq!(
            cluster.calls(),
            ["delete configmap blitz/polar-ca", "delete configmap polar/polar-ca", "delete secret polar/cert-issuer-ca"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_ca_is_shared_so_down_leaves_it_unless_the_core_is_asked_for() {
        let cluster = Fake::default();
        run_ca(&cluster, fast()).await.0.unwrap();
        cluster.calls.borrow_mut().clear();

        let plan = Plan::from_yaml(CA_PLAN).unwrap();
        down(&cluster, &plan, false, &mut |_| {}).await.unwrap();
        assert_eq!(cluster.calls(), ["delete configmap blitz/polar-ca"], "the application's pin goes; the core's CA stays");
        assert!(cluster.secrets.borrow().contains_key(&("polar".to_string(), "cert-issuer-ca".to_string())));
    }
}
