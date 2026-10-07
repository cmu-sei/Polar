//! The catalog and the code must agree about the numbers they share.
//!
//! `infra/config/kinds.nuon` is where a deployment says how much time an agent has to
//! rejuvenate, how stale its health file may get, and how long it has to drain. The
//! same numbers are the defaults in the Rust code, and `infra/rules.nu` checks them
//! together at render time on the assumption that they are the same. These tests make
//! that assumption true: change one side without the other and a build fails, which
//! turns "someone quietly edited a number" into a conversation about both.
//!
//! They read the catalog from the source tree. A build that does not have it (a
//! sandbox that copies only the crate) skips them, and says so.

use polar_agent_builder::DEFAULT_SHUTDOWN_BUDGET;
use std::fs;
use std::path::PathBuf;

fn infra_config() -> Option<PathBuf> {
    let infra = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../infra");
    if !infra.is_dir() {
        eprintln!("skipping: {} is not in this source tree", infra.display());
        return None;
    }
    Some(infra.join("config"))
}

/// The text of the `block: { ... }` entry of the `agent` kind.
fn block(kinds: &str, name: &str) -> String {
    let start = kinds
        .find(&format!("{name}: {{"))
        .unwrap_or_else(|| panic!("kinds.nuon has no `{name}` block"));
    let open = start + kinds[start..].find('{').unwrap();
    let mut depth = 0;
    for (i, c) in kinds[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return kinds[open..open + i + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("`{name}` block in kinds.nuon is not closed");
}

/// The whole number after `key:` in `text`.
fn number(text: &str, key: &str) -> u64 {
    let after = text
        .find(&format!("{key}:"))
        .unwrap_or_else(|| panic!("kinds.nuon has no `{key}`"))
        + key.len()
        + 1;
    text[after..]
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("`{key}` in kinds.nuon is not a whole number"))
}

fn kinds() -> Option<String> {
    let path = infra_config()?.join("kinds.nuon");
    Some(
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display())),
    )
}

#[test]
fn the_catalog_health_settings_are_the_defaults_the_code_has() {
    let Some(kinds) = kinds() else { return };
    let health = block(&kinds, "health");

    assert_eq!(
        number(&health, "tick_secs"),
        polar_health::DEFAULT_TICK_SECS
    );
    assert_eq!(
        number(&health, "expiry_secs"),
        polar_health::DEFAULT_EXPIRY_THRESHOLD_SECS as u64
    );
    assert_eq!(
        number(&health, "rejuvenation_secs"),
        polar_health::DEFAULT_REJUVENATION_THRESHOLD_SECS as u64
    );
    assert_eq!(
        number(&health, "stale_secs"),
        polar_health::DEFAULT_STALE_THRESHOLD_SECS
    );
}

#[test]
fn the_catalog_jitter_window_is_the_one_the_health_actor_uses() {
    // The only number the rules need that is not in the agent's environment: it is a
    // constant in the health actor. If it changes, the room a rejuvenation needs
    // changes with it, and the catalog has to be looked at.
    let Some(kinds) = kinds() else { return };
    assert_eq!(
        number(&block(&kinds, "health"), "jitter_window_secs"),
        polar_health::DEFAULT_JITTER_WINDOW_SECS
    );
}

#[test]
fn the_catalog_default_budget_is_the_builders_default() {
    let Some(kinds) = kinds() else { return };
    assert_eq!(
        number(&block(&kinds, "shutdown"), "budget_secs"),
        DEFAULT_SHUTDOWN_BUDGET.as_secs()
    );
}

#[test]
fn the_helpers_read_what_they_should() {
    let sample = "agent: {\n  health: {\n    tick_secs: 30\n    nested: { x: 1 }\n    stale_secs: 90\n  }\n  other: { stale_secs: 5 }\n}";
    let health = block(sample, "health");
    assert_eq!(number(&health, "tick_secs"), 30);
    assert_eq!(
        number(&health, "stale_secs"),
        90,
        "not the one in the next block"
    );
}
