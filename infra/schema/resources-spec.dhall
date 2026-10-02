-- infra/schema/resources-spec.dhall
--
-- What `infra/resolve.nu resources <workload>` produces: the Kubernetes
-- objects that live alongside a workload, contributed by its capabilities
-- (see the `resources` facet in config/capabilities.nuon).
--
-- Plain Text and Lists only, for the same reasons as agent-spec.dhall.

let Rule =
      { api_groups : List Text, resources : List Text, verbs : List Text }

let ClusterRoleEntry = { name : Text, rules : List Rule }

let Spec =
      { name : Text
      , namespace : Text
      , service_account : Text
      , cluster_roles : List ClusterRoleEntry
      , sa_token_secrets : List Text
      }

in  { Spec, Rule, ClusterRoleEntry }
