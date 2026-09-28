-- infra/layers/2-services/credentials-agent/deployment.dhall
--
-- Credentials Agent Job. Structurally a Job, not a Deployment,
-- matching this codebase's own, deliberate, established convention
-- for every long-running agent: Kubernetes does not, as of this
-- cluster's own version, re-run init containers on a Deployment's
-- own in-place pod restart -- only on genuine pod recreation, which
-- a Job with a huge backoffLimit guarantees on every restart. This
-- matters specifically here: polar-init's own real, new escrow-
-- keypair bootstrap step (see polar-crypto's own commit history)
-- needs to run on every restart the same way mTLS cert issuance
-- already does.

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall
let functions = ../../../schema/functions.dhall

let render =
      \(v :
          { name             : Text
          , image            : Text
          , imagePullPolicy  : Text
          , imagePullSecrets : List { name : Optional Text }
          , polarInitImage   : Text
          , certIssuerUrl    : Text
          , saTokenAudience  : Text
          , neo4jBoltAddr    : Text
          , dataVolumeName   : Text
          }
      ) ->
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , k8s.Volume::{ name = Constants.neo4jClientCertVolumeName, emptyDir = Some k8s.EmptyDirVolumeSource::{=} }
              , k8s.Volume::{ name = "polar-health", emptyDir = Some k8s.EmptyDirVolumeSource::{=} }
              , k8s.Volume::{
                , name                  = v.dataVolumeName
                , persistentVolumeClaim = Some k8s.PersistentVolumeClaimVolumeSource::{ claimName = "${v.name}-data" }
                }
              ]

        let env =
              Constants.commonClientEnv
              # functions.makeGraphEnv v.neo4jBoltAddr Constants.graphConfig Constants.graphSecretKeySelector (Some "/etc/neo4j-client-tls/ca.pem")
              # [ k8s.EnvVar::{ name = "CREDENTIALS_AGENT_CERT_ISSUER_URL", value = Some v.certIssuerUrl }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_FILE",          value = Some "/var/run/polar-health/polar-health.json" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_CERTS",         value = Some "/etc/tls/certs/cert.pem,/etc/neo4j-client-tls/cert.pem" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS",   value = Some "60" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",     value = Some "30" }
                , k8s.EnvVar::{ name = "POLAR_HEALTH_DEP_ENDPOINTS", value = Some "${Constants.cassiniDNSName}:${Natural/show Constants.cassiniPort}:300,${Constants.neo4jDNSName}:7687:300" }
                ]

        let mounts =
              [ Constants.certVolumeMount
              , k8s.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath, readOnly = Some True }
              , k8s.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              , k8s.VolumeMount::{ name = v.dataVolumeName, mountPath = "/var/lib/credentials-agent" }
              ]

        let podSpec = k8s.PodSpec::{
              , imagePullSecrets = Some v.imagePullSecrets
              , serviceAccountName = Some v.name
              , automountServiceAccountToken = Some False
              , restartPolicy    = Some "Never"
              , volumes          = Some volumes
              , initContainers   = Some
                [ functions.makePolarInitContainer
                    v.polarInitImage
                    v.imagePullPolicy
                    Constants.saTokenVolumeName
                    v.certIssuerUrl
                    Constants.saTokenPath
                    [ k8s.VolumeMount::{ name = Constants.certVolumeName,            mountPath = Constants.tlsPath             }
                    , k8s.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath }
                    ]
                    ([] : List Text)
                    [ "client:${Constants.tlsPath}:ecdsa-p256:"
                    , "client:${Constants.neo4jClientCertPath}:ecdsa-p256:"
                    ]
                    -- The one, genuine core-service exception this
                    -- codebase's own escrow-bootstrap circularity
                    -- requires: CryptoActor is the single gateway to
                    -- the Credentials Agent, so the Credentials
                    -- Agent's own polar-init instance can't depend
                    -- on it existing yet. See polar-crypto's own
                    -- commit history for the complete reasoning.
                    True
                ]
              , containers =
                [ k8s.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  , env             = Some env
                  , volumeMounts    = Some mounts
                  , livenessProbe   = Some k8s.Probe::{
                      , exec                = Some { command = Some [ "polar-healthcheck" ] }
                      , initialDelaySeconds = Some 150
                      , periodSeconds       = Some 30
                      , failureThreshold    = Some 2
                      , timeoutSeconds      = Some 5
                      }
                  }
                ]
              }

        in  k8s.Job::{
            , metadata = k8s.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some k8s.JobSpec::{
              , backoffLimit            = Some 1000000
              , parallelism             = Some 1
              , completions             = Some 1
              , ttlSecondsAfterFinished = Some 300
              , template                = k8s.PodTemplateSpec::{
                , metadata = Some k8s.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec     = Some podSpec
                }
              }
            }

in render
