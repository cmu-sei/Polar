-- infra/layers/2-services/ace/job.dhall
--
-- ACE Job and Service.
--
-- ACE runs as a Job, not a Deployment, for the same reason every other
-- Polar workload does: a Job's pod is fully replaced on completion/retry,
-- which re-runs the polar-init init container and issues a fresh
-- certificate. A Deployment's in-place container restart does not.
--
-- ACE requests a one-year certificate (see identityLifetimeOverrides in
-- the target's overrides.dhall) -- day to day, this means ACE almost
-- never needs a new cert and so almost never needs a fresh pod for that
-- reason. But if something DOES go wrong (cert corruption, environment
-- drift, cert-issuer config changes), a fresh pod via a Job restart is
-- the only way ACE can recover a working certificate at all. Being a Job
-- is what keeps that recovery path available, even though it's rarely
-- exercised in normal operation.
--
-- Liveness probe / self-exit split:
--   - The Kubernetes exec liveness probe (polar_healthcheck.py) checks
--     ONE thing: is the heartbeat file fresh. That is the only failure
--     class Kubernetes needs to catch -- the background health-monitor
--     thread (utils/health.py) has stopped ticking entirely and the
--     whole process is wedged.
--   - Every other health judgment -- graph connectivity -- is the
--     service's own responsibility. GraphHealthMonitor decides for
--     itself when the graph has been unreachable long enough to warrant
--     a fresh pod, and exits the process directly (os._exit) rather
--     than reporting "unhealthy" and waiting for Kubernetes to notice
--     via the probe.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall
let functions  = ../../../schema/functions.dhall

let render =
      \(v :
          { name               : Text
          , image              : Text
          , imagePullPolicy    : Text
          , imagePullSecrets   : List { name : Optional Text }
          , serviceAccountName : Text
          , serviceType        : Text
          , port               : Natural
          , polarInitImage     : Text
          , credentialsObserverUrl : Text
          , saTokenAudience    : Text
          , neo4jBoltAddr      : Text
          }
      ) ->

        let volumes =
              [ kubernetes.Volume::{
                , name     = Constants.neo4jClientCertVolumeName
                , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
                }
              , Constants.saTokenVolume v.saTokenAudience
              , kubernetes.Volume::{
                , name     = "polar-health"
                , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
                }
              ]

        let env =
              functions.makeGraphEnv
                v.neo4jBoltAddr
                Constants.graphConfig
                Constants.graphSecretKeySelector
                (Some "/etc/neo4j-client-tls/ca.pem")
              # [ kubernetes.EnvVar::{ name = "DASH_HOST", value = Some "0.0.0.0" }
                , kubernetes.EnvVar::{ name = "DASH_PORT", value = Some (Natural/show v.port) }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_FILE",                value = Some "/var/run/polar-health/polar-health.json" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_STALE_SECS",          value = Some "90" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",           value = Some "30" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_GRAPH_DEADLINE_SECS", value = Some "120" }
                ]

        let mounts =
              [ kubernetes.VolumeMount::{
                , name      = Constants.neo4jClientCertVolumeName
                , mountPath = Constants.neo4jClientCertPath
                , readOnly  = Some True
                }
              , kubernetes.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              ]

        let podSpec = kubernetes.PodSpec::{
              , serviceAccountName = Some v.serviceAccountName
              , imagePullSecrets   = Some v.imagePullSecrets
              , restartPolicy      = Some "Never"
              , volumes            = Some volumes
              , initContainers     = Some
                [ functions.makePolarInitContainer
                    v.polarInitImage
                    v.imagePullPolicy
                    Constants.saTokenVolumeName
                    ""
                    (functions.credentialsAgentIssuerEnv v.credentialsObserverUrl)
                    Constants.saTokenPath
                    [ kubernetes.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath } ]
                    ([] : List Text)
                    [ "client:${Constants.neo4jClientCertPath}:ecdsa-p256:" ]
                    True
                ]
              , containers =
                [ kubernetes.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  , env             = Some env
                  , volumeMounts    = Some mounts
                  , ports           = Some
                    [ kubernetes.ContainerPort::{ containerPort = v.port } ]
                  , livenessProbe   = Some kubernetes.Probe::{
                      , exec                = Some { command = Some [ "polar-healthcheck" ] }
                      , initialDelaySeconds = Some 30
                      , periodSeconds       = Some 30
                      , failureThreshold    = Some 2
                      , timeoutSeconds      = Some 5
                      }
                  }
                ]
              }

        let job =
              kubernetes.Job::{
              , metadata = kubernetes.ObjectMeta::{
                , name        = Some v.name
                , namespace   = Some Constants.PolarNamespace
                , annotations = Some [ Constants.RejectSidecarAnnotation ]
                }
              , spec = Some kubernetes.JobSpec::{
                , backoffLimit            = Some 1000000
                , podReplacementPolicy = Some Constants.JobPodReplacementPolicy
                , parallelism             = Some 1
                , completions             = Some 1
                , ttlSecondsAfterFinished = Some 300
                , template                = kubernetes.PodTemplateSpec::{
                  , metadata = Some kubernetes.ObjectMeta::{
                    , name   = Some v.name
                    , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                    }
                  , spec = Some podSpec
                  }
                }
              }

        let service =
              kubernetes.Service::{
              , metadata = kubernetes.ObjectMeta::{
                , name      = Some "${v.name}-svc"
                , namespace = Some Constants.PolarNamespace
                }
              , spec = Some kubernetes.ServiceSpec::{
                , selector = Some (toMap { name = v.name })
                , type     = Some v.serviceType
                , ports    = Some
                  [ kubernetes.ServicePort::{
                    , name       = Some "http"
                    , targetPort = Some (kubernetes.NatOrString.Nat v.port)
                    , port       = v.port
                    }
                  ]
                }
              }

        in  [ kubernetes.Resource.Job     job
            , kubernetes.Resource.Service service
            ]

in render
