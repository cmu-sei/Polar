//! Which configuration a discovered repository is observed with: the one stored in the graph, or a
//! default for a repository nothing has been said about.

use git_agent_common::RepoObservationConfig;
use polar::graph::controller::{GraphController, GraphControllerMsg};
use polar::graph::nodes::git::RepoId;

pub const DEFAULT_REMOTE: &str = "origin";
pub const DEFAULT_SHALLOW_DEPTH: usize = 100;
pub const DEFAULT_REF: &str = "refs/heads/main";

/// What a repository is observed with when the graph holds nothing for it.
pub fn default_config(repo_id: RepoId, repo_url: String) -> RepoObservationConfig {
    RepoObservationConfig::new(
        repo_id,
        repo_url,
        vec![DEFAULT_REMOTE.to_string()],
        Some(DEFAULT_SHALLOW_DEPTH),
        vec![DEFAULT_REF.to_string()],
    )
}

/// The stored configuration for a repository, if any, read through the graph controller.
///
/// The graph-stored config does not yet include credentials: they will be sourced from the
/// credentials agent at dispatch time. Until then `credential` is always `None`, so the result is
/// suitable for public repositories only.
pub async fn fetch_repo_observation_config(
    controller: &GraphController,
    repo_id: &RepoId,
) -> Result<Option<RepoObservationConfig>, String> {
    let query = neo4rs::query(REPO_CONFIG_QUERY).param("repo_id", repo_id.to_string());

    let rows = match ractor::rpc::call(
        controller,
        |reply| GraphControllerMsg::Query(query, reply),
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(Ok(rows))) => rows,
        Ok(ractor::rpc::CallResult::Success(Err(e))) => return Err(e),
        other => return Err(format!("the graph controller did not answer: {other:?}")),
    };
    config_from_rows(&rows)
}

/// What [`fetch_repo_observation_config`] asks the graph; `$repo_id` is the repository's.
pub const REPO_CONFIG_QUERY: &str = r#"
        MATCH (c:RepoObservationConfig { repo_id: $repo_id })
        RETURN
            c.repo_id        AS repo_id,
            c.repo_url       AS repo_url,
            c.remotes        AS remotes,
            c.max_depth      AS max_depth,
            c.tracked_refs   AS tracked_refs
        LIMIT 1
    "#;

/// The configuration in the rows [`REPO_CONFIG_QUERY`] returned, if there is one.
pub fn config_from_rows(rows: &[neo4rs::Row]) -> Result<Option<RepoObservationConfig>, String> {
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let get = |e: neo4rs::DeError| e.to_string();
    let repo_id: String = row.get("repo_id").map_err(get)?;
    let repo_url: String = row.get("repo_url").map_err(get)?;
    let remotes: Vec<String> = row.get("remotes").map_err(get)?;
    // The stored `max_depth` has always been handed on as the shallow fetch depth, which is what
    // the constructor's fourth argument is. Whether the node should say `shallow_depth` is for the
    // scheduler's redesign.
    let max_depth: Option<i64> = row.get("max_depth").map_err(get)?;
    let tracked_refs: Vec<String> = row.get("tracked_refs").map_err(get)?;
    Ok(Some(RepoObservationConfig::new(
        RepoId::new(repo_id),
        repo_url,
        remotes,
        max_depth.map(|v| v as usize),
        tracked_refs,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_repository_is_observed_on_main_from_origin() {
        let config = default_config(RepoId::new("r".into()), "https://example.org/r.git".into());
        assert_eq!(config.repo_url, "https://example.org/r.git");
        assert_eq!(config.remotes, [DEFAULT_REMOTE]);
        assert_eq!(config.refs, [DEFAULT_REF]);
        assert_eq!(config.shallow_depth, Some(DEFAULT_SHALLOW_DEPTH));
        assert!(config.credential.is_none());
    }
}
