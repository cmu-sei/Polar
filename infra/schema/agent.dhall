-- infra/schema/agent.dhall
--
-- The one Job template every catalog agent shares (observers, consumers,
-- processors). It holds structure only. Every fact (which env, which
-- volumes, which dependency checks) arrives in the Spec, resolved from
-- infra/config by infra/resolve.nu. Nothing here branches on which agent it
-- is; variation is data.
--
-- If a workload does not fit this shape, it stays a custom chart (the escape
-- hatch). Do not add a Bool to the Spec to make it fit.

let kubernetes = ./kubernetes.dhall

let Constants = ./constants.dhall

let functions = ./functions.dhall

let T = ./agent-spec.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let render =
      λ(v : T.Spec) →
        let toEnv =
              λ(e : T.EnvEntry) →
                kubernetes.EnvVar::{
                , name = e.name
                , value = e.value
                , valueFrom =
                    Prelude.Optional.map
                      Text
                      kubernetes.EnvVarSource.Type
                      ( λ(secretName : Text) →
                          kubernetes.EnvVarSource::{
                          , secretKeyRef = Some kubernetes.SecretKeySelector::{
                            , name = Some secretName
                            , key = Prelude.Optional.default Text "" e.key
                            }
                          }
                      )
                      e.secret
                }

        let toVolume =
              λ(x : T.VolumeEntry) →
                kubernetes.Volume::{
                , name = x.name
                , emptyDir =
                    if    Prelude.Optional.null Text x.secret
                       && Prelude.Optional.null Text x.audience
                       && Prelude.Optional.null Text x.config_map
                       && Prelude.Optional.null Text x.claim
                    then  Some kubernetes.EmptyDirVolumeSource::{=}
                    else  None kubernetes.EmptyDirVolumeSource.Type
                , secret =
                    Prelude.Optional.map
                      Text
                      kubernetes.SecretVolumeSource.Type
                      ( λ(secretName : Text) →
                          kubernetes.SecretVolumeSource::{
                          , secretName = Some secretName
                          , items =
                              if    Prelude.List.null { key : Text, path : Text } x.items
                              then  None (List kubernetes.KeyToPath.Type)
                              else  Some
                                      ( Prelude.List.map
                                          { key : Text, path : Text }
                                          kubernetes.KeyToPath.Type
                                          ( λ(i : { key : Text, path : Text }) →
                                              kubernetes.KeyToPath::{ key = i.key, path = i.path }
                                          )
                                          x.items
                                      )
                          }
                      )
                      x.secret
                , configMap =
                    Prelude.Optional.map
                      Text
                      kubernetes.ConfigMapVolumeSource.Type
                      ( λ(configMapName : Text) →
                          kubernetes.ConfigMapVolumeSource::{
                          , name = Some configMapName
                          }
                      )
                      x.config_map
                , persistentVolumeClaim =
                    Prelude.Optional.map
                      Text
                      kubernetes.PersistentVolumeClaimVolumeSource.Type
                      ( λ(claimName : Text) →
                          kubernetes.PersistentVolumeClaimVolumeSource::{
                          , claimName = claimName
                          }
                      )
                      x.claim
                , projected =
                    Prelude.Optional.map
                      Text
                      kubernetes.ProjectedVolumeSource.Type
                      ( λ(audience : Text) →
                          kubernetes.ProjectedVolumeSource::{
                          , sources = Some
                            [ kubernetes.VolumeProjection::{
                              , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                                , path = "token"
                                , audience = Some audience
                                , expirationSeconds = Some
                                    v.sa_token_expiration_secs
                                }
                              }
                            ]
                          }
                      )
                      x.audience
                }

        let toMount =
              λ(m : T.MountEntry) →
                kubernetes.VolumeMount::{
                , name = m.name
                , mountPath = m.path
                , readOnly = if m.read_only then Some True else None Bool
                }

        let toInitMount =
              λ(m : T.InitMountEntry) →
                kubernetes.VolumeMount::{ name = m.name, mountPath = m.path }

        let volumes =
                Prelude.List.map T.VolumeEntry kubernetes.Volume.Type toVolume v.volumes
              # functions.ProxyVolume v.proxy_ca_cert

        let env =
                Prelude.List.map T.EnvEntry kubernetes.EnvVar.Type toEnv v.env
              # functions.ProxyEnv v.proxy_ca_cert

        let mounts =
                Prelude.List.map T.MountEntry kubernetes.VolumeMount.Type toMount v.mounts
              # functions.ProxyMount v.proxy_ca_cert

        let podSpec =
              kubernetes.PodSpec::{
              , imagePullSecrets = Some v.pull_secrets
              , serviceAccountName = Some v.service_account
              , automountServiceAccountToken = v.automount_sa_token
              , volumes = Some volumes
              , restartPolicy = Some v.restart_policy
              , terminationGracePeriodSeconds = Some v.termination_grace_secs
              , initContainers = Some
                [ functions.makePolarInitContainer
                    v.polar_init_image
                    v.pull_policy
                    v.sa_token_volume
                    v.cert_issuer_url
                    ( Prelude.List.map
                        T.EnvEntry
                        kubernetes.EnvVar.Type
                        toEnv
                        v.init_env
                    )
                    v.sa_token_path
                    ( Prelude.List.map
                        T.InitMountEntry
                        kubernetes.VolumeMount.Type
                        toInitMount
                        v.init_mounts
                    )
                    v.dep_checks
                    v.certs
                    False
                ]
              , containers =
                [ kubernetes.Container::{
                  , name = v.name
                  , image = Some v.image
                  , imagePullPolicy = Some v.pull_policy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  , env = Some env
                  , ports =
                      if    Prelude.List.null Natural v.ports
                      then  None (List kubernetes.ContainerPort.Type)
                      else  Some
                              ( Prelude.List.map
                                  Natural
                                  kubernetes.ContainerPort.Type
                                  ( λ(port : Natural) →
                                      kubernetes.ContainerPort::{ containerPort = port }
                                  )
                                  v.ports
                              )
                  , volumeMounts = Some mounts
                  , livenessProbe = Some kubernetes.Probe::{
                    , exec = Some { command = Some [ "polar-healthcheck" ] }
                    , initialDelaySeconds = Some v.probe.initial_delay_secs
                    , periodSeconds = Some v.probe.period_secs
                    , failureThreshold = Some v.probe.failure_threshold
                    , timeoutSeconds = Some v.probe.timeout_secs
                    }
                  }
                ]
              }

        in  kubernetes.Job::{
            , metadata = kubernetes.ObjectMeta::{
              , name = Some v.name
              , namespace = Some v.namespace
              , annotations = Some [ Constants.RejectSidecarAnnotation ]
              }
            , spec = Some kubernetes.JobSpec::{
              , backoffLimit = Some v.backoff_limit
              , podReplacementPolicy = Some Constants.JobPodReplacementPolicy
              , parallelism = Some 1
              , completions = Some 1
              , ttlSecondsAfterFinished = Some v.ttl_after_finished_secs
              , template = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{
                  , name = Some v.name
                  , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                  }
                , spec = Some podSpec
                }
              }
            }

in  render
