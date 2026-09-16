-- infra/layers/2-services/credentials-agent/pvc.dhall
-- PVC for the Credentials Agent's own ledger event log and escrow
-- keypair. Must survive pod restarts -- emptyDir is not acceptable:
-- losing the escrow keypair specifically would permanently orphan
-- every secret ever escrowed system-wide; losing the event log would
-- discard the actual, real source of truth this agent is built
-- around. Mirrors cert-issuer's own pvc.dhall exactly -- the same,
-- established pattern for "must persist" storage in this codebase.

let Constants = ../../../schema/constants.dhall

let render =
      \(v : { name : Text, dataVolumeName : Text, dataStorageClass : Text, dataStorageSize : Text }) ->
        { apiVersion = "v1"
        , kind       = "PersistentVolumeClaim"
        , metadata   =
          { name      = "${v.name}-data"
          , namespace = Constants.PolarNamespace
          }
        , spec =
          { accessModes      = [ "ReadWriteOnce" ]
          , storageClassName = v.dataStorageClass
          , resources.requests.storage = v.dataStorageSize
          }
        }

in render
