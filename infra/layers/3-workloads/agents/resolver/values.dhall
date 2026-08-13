-- infra/layers/3-workloads/agents/resolver/values.dhall
--
-- Canonical defaults for the resolver agent chart.
-- Resolves OCI registry references; needs docker config mounted.
-- Promoted out of provenance/ to top-level -- no longer part of a
-- combined provenance/{linker,resolver} pairing.

let Constants = ../../../../schema/constants.dhall

in  { name             = Constants.RegistryResolverName
    , imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , resolver =
      { name  = Constants.RegistryResolverName
      , image = "oci-resolver:latest"
      }

    , proxyCACert = None Text
    }
