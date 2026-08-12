-- infra/layers/2-services/ace/rbac.dhall
--
-- ACE's dedicated ServiceAccount.
--
-- ACE only talks to Neo4j -- it never calls the Kubernetes API -- so
-- unlike kube-observer's rbac.dhall this needs no ClusterRole or
-- ClusterRoleBinding. The ServiceAccount exists solely to give ACE its
-- own workload identity for cert-issuer, distinct from `default` in the
-- polar namespace, so its one-year certificate lifetime override
-- (identityLifetimeOverrides) applies only to ACE and not to every other
-- workload sharing the default ServiceAccount.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v : { serviceAccountName : Text }) ->

        kubernetes.ServiceAccount::{
        , apiVersion = "v1"
        , kind       = "ServiceAccount"
        , metadata   = kubernetes.ObjectMeta::{
          , name      = Some v.serviceAccountName
          , namespace = Some Constants.PolarNamespace
          }
        , automountServiceAccountToken = Some False
        }

in render
