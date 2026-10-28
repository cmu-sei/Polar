-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

-- infra/schema/ca.dhall
--
-- The template of the `ca` kind, cert-issuer's alone (docs/architecture/cert-issuer-isolation.md, step 1c): the
-- agent template's Job (agent.dhall) without polar-init, since cert-issuer is the issuer, and with what only its pod
-- has: the kind's projected token mounted in its own container, on which it signs itself its client certificate
-- through its own floor; the Kubernetes API token; and the pod's group, by which its CA's Secret is readable. It holds
-- structure only, as agent.dhall does: every fact arrives in the Spec (ca-spec.dhall), resolved by infra/resolve.nu.

let kubernetes = ./kubernetes.dhall

let Constants = ./constants.dhall

let functions = ./functions.dhall

let A = ./agent-spec.dhall

let T = ./ca-spec.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let render =
      λ(v : T.Spec) →
        let toEnv =
              λ(e : A.EnvEntry) →
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
                          , defaultMode = x.mode
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
              λ(m : A.MountEntry) →
                kubernetes.VolumeMount::{
                , name = m.name
                , mountPath = m.path
                , readOnly = if m.read_only then Some True else None Bool
                }

        let volumes =
                Prelude.List.map T.VolumeEntry kubernetes.Volume.Type toVolume v.volumes
              # functions.ProxyVolume v.proxy_ca_cert

        let env =
                Prelude.List.map A.EnvEntry kubernetes.EnvVar.Type toEnv v.env
              # functions.ProxyEnv v.proxy_ca_cert

        -- Its own token, the kind's projected one, where polar-init would have it (functions.dhall): cert-issuer signs
        -- itself its client certificate on it.
        let mounts =
                Prelude.List.map A.MountEntry kubernetes.VolumeMount.Type toMount v.mounts
              # [ kubernetes.VolumeMount::{
                  , name = v.sa_token_volume
                  , mountPath = "/workspace"
                  , readOnly = Some True
                  }
                ]
              # functions.ProxyMount v.proxy_ca_cert

        let placed = functions.placementOf v.placement

        let podSpec =
              kubernetes.PodSpec::{
              , nodeSelector = placed.nodeSelector
              , tolerations = placed.tolerations
              , imagePullSecrets = Some v.pull_secrets
              , serviceAccountName = Some v.service_account
              -- The Kubernetes API token: it fetches the API server's keys to validate tokens.
              , automountServiceAccountToken = Some True
              -- Its files, the CA's Secret among them, are readable by this group.
              , securityContext =
                  Prelude.Optional.map
                    Natural
                    kubernetes.PodSecurityContext.Type
                    (λ(group : Natural) → kubernetes.PodSecurityContext::{ fsGroup = Some group })
                    v.fs_group
              , volumes = Some volumes
              , restartPolicy = Some v.restart_policy
              , terminationGracePeriodSeconds = Some v.termination_grace_secs
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
