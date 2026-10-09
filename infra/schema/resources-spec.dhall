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

-- A ConfigMap with one key: the document a workload's agent reads.
let ConfigMapEntry = { name : Text, key : Text, content : Text }

-- A persistent volume claim a workload's capabilities ask for.
let PvcEntry = { name : Text, storage_class : Text, size : Text }

-- A Service in front of the workload's pod, on one port.
let ServiceEntry = { name : Text, port : Natural }

let Spec =
      { name : Text
      , namespace : Text
      , service_account : Text
      , cluster_roles : List ClusterRoleEntry
      , sa_token_secrets : List Text
      , config_maps : List ConfigMapEntry
      , pvcs : List PvcEntry
      , services : List ServiceEntry
      }

in  { Spec, Rule, ClusterRoleEntry, ConfigMapEntry, PvcEntry, ServiceEntry }
