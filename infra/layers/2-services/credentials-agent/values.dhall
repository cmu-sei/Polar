-- infra/layers/2-services/credentials-agent/values.dhall

{ name             = "credentials-agent"
, image            = "credentials-agent-consumer:latest"
, observerName     = "credentials-agent-observer"
, observerImage    = "credentials-agent-observer:latest"
, observerPort     = 8080
, imagePullPolicy  = "IfNotPresent"
, imagePullSecrets = [] : List { name : Optional Text }
, dataVolumeName   = "credentials-agent-data"
, dataStorageClass = "managed-csi"
, dataStorageSize  = "1Gi"
}
