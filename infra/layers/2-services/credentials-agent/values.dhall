-- infra/layers/2-services/credentials-agent/values.dhall

{ name             = "credentials-agent"
    , image            = "credentials-agent-consumer:latest"
    , imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }
    , dataVolumeName   = "credentials-agent-data"
    , dataStorageClass = "managed-csi"
    , dataStorageSize  = "1Gi"
    }
