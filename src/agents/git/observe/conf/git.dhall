-- git.dhall
--
-- Source of truth for the git observer agent's per-repo HTTP credential
-- configuration (POLAR_GIT_AGENT_CONFIG). Compile to YAML with:
--
--   dhall-to-yaml --file git.dhall --output git.yaml
--
-- Corresponds to git_agent_common::{GitAgentConfig, RepoConfig,
-- HttpCredentialConfig}. Keys under `repos` are repo URLs; they get
-- normalized (scheme/host lowercased, trailing `.git` and `/` stripped) by
-- GitAgentConfig::load before being used as lookup keys, so it doesn't
-- matter whether you write `.git` suffixes or trailing slashes here.

-- ===========================================================================
-- Types
-- ===========================================================================

-- Maps to HttpCredentialConfig. `username` overrides the dummy username used
-- for token auth (default "oauth2" -- see GitHttpCredential::as_userpass).
-- Needed for forges like Gitea where token auth doesn't accept "oauth2".
let HttpCredential = { token : Text, username : Optional Text }

let RepoConfig = { http : Optional HttpCredential }

let RepoEntry = { mapKey : Text, mapValue : RepoConfig }

let GitAgentConfig = { repos : List RepoEntry }

let noAuth
    : RepoConfig
    = { http = None HttpCredential }

let tokenAuth
    : Text -> RepoConfig
    = \(token : Text) ->
        { http = Some { token, username = None Text } : Optional HttpCredential
        }

let tokenAuthAs
    : Text -> Text -> RepoConfig
    = \(token : Text) ->
      \(username : Text) ->
        { http =
            Some { token, username = Some username } : Optional HttpCredential
        }

in  { GitAgentConfig, tokenAuth, tokenAuthAs, noAuth }
