-- infra/layers/3-workloads/agents/provenance/values.dhall
--
-- Canonical defaults for the provenance agent chart.
-- Linker: reads graph data and links provenance artifacts.
-- Resolver: resolves OCI registry references, needs docker config mounted.
--
-- Both deployments reject Istio sidecar injection unconditionally.

let Constants = ../../../../schema/constants.dhall

in  { imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , linker =
      { name  = Constants.ProvenanceLinkerName
      , image = "provenance-linker-agent:latest"
      }

    , resolver =
      { name  = Constants.ProvenanceResolverName
      , image = "provenance-resolver-agent:latest"
      }

    , proxyCACert = None Text
    }
