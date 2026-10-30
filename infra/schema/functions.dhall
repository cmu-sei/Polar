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

let kubernetes =
      ./kubernetes.dhall
        sha256:2086df2178fdb83a08bd4ca89c951f12e4ce16d3cc1441005fcda943a028bb8c

let Constants =
      ./constants.dhall

let Agents =
      ./agents.dhall
        sha256:8458543ac92aa19c950efdf97f5d89c85fbf3be94daad98d716c3217deddd570

let GraphConfig = Agents.GraphConfig

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

-- `certDir` is where the client certificate Neo4j is shown lives: a pod's one client certificate, presented to Cassini
-- and Neo4j alike (infra/config/capabilities.nuon, graph-write), or a job's own.
let makeGraphEnv
    : Text →
      GraphConfig →
      kubernetes.SecretKeySelector.Type →
      Text →
      Optional Text →
        List kubernetes.EnvVar.Type
    = λ(addr : Text) →
      λ(g : GraphConfig) →
      λ(passwordSecret : kubernetes.SecretKeySelector.Type) →
      λ(certDir : Text) →
      λ(caCertPath : Optional Text) →
          [ kubernetes.EnvVar::{ name = "GRAPH_ENDPOINT", value = Some addr }
          , kubernetes.EnvVar::{ name = "GRAPH_DB", value = Some g.graphDB }
          , kubernetes.EnvVar::{
            , name = "GRAPH_USER"
            , value = Some g.graphUsername
            }
          , kubernetes.EnvVar::{
            , name = "GRAPH_PASSWORD"
            , valueFrom = Some kubernetes.EnvVarSource::{
              , secretKeyRef = Some passwordSecret
              }
            }
          , kubernetes.EnvVar::{
            , name = "GRAPH_CLIENT_CERT"
            , value = Some "${certDir}/cert.pem"
            }
          , kubernetes.EnvVar::{
            , name = "GRAPH_CLIENT_KEY"
            , value = Some "${certDir}/key.pem"
            }
          ]
        # merge
            { Some =
                λ(path : Text) →
                  [ kubernetes.EnvVar::{
                    , name = "GRAPH_CA_CERT"
                    , value = Some path
                    }
                  ]
            , None = [] : List kubernetes.EnvVar.Type
            }
            caCertPath

let makeOpaqueSecret
    : Text → Text → Text → kubernetes.Secret.Type
    = λ(name : Text) →
      λ(key : Text) →
      λ(value : Text) →
        kubernetes.Secret::{
        , apiVersion = "v1"
        , kind = "Secret"
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some Constants.PolarNamespace
          }
        , stringData = Some [ { mapKey = key, mapValue = value } ]
        , type = Some "Opaque"
        }

let makeDeployment
    : ∀(name : Text) → kubernetes.PodSpec.Type → kubernetes.Deployment.Type
    = λ(name : Text) →
      λ(podSpec : kubernetes.PodSpec.Type) →
        kubernetes.Deployment::{
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some Constants.PolarNamespace
          }
        , spec = Some kubernetes.DeploymentSpec::{
          , selector = kubernetes.LabelSelector::{
            , matchLabels = Some (toMap { name })
            }
          , replicas = Some 1
          , template = kubernetes.PodTemplateSpec::{
            , metadata = Some kubernetes.ObjectMeta::{
              , name = Some name
              , labels = Some [ { mapKey = "name", mapValue = name } ]
              }
            , spec = Some podSpec
            }
          }
        }

let ProxyVolume =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(certName : Text) →
                [ kubernetes.Volume::{
                  , name = certName
                  , secret = Some kubernetes.SecretVolumeSource::{
                    , secretName = Some certName
                    }
                  }
                ]
          , None = [] : List kubernetes.Volume.Type
          }
          cert

let ProxyMount =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(cert : Text) →
                [ kubernetes.VolumeMount::{
                  , name = cert
                  , mountPath = "/etc/tls/proxy"
                  , readOnly = Some True
                  }
                ]
          , None = [] : List kubernetes.VolumeMount.Type
          }
          cert

let ProxyEnv =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(_ : Text) →
                [ kubernetes.EnvVar::{
                  , name = "PROXY_CA_CERT"
                  , value = Some "/etc/tls/proxy/proxy.crt"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert

let GraphProxyEnv =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(_ : Text) →
                [ kubernetes.EnvVar::{
                  , name = "GRAPH_CA_CERT"
                  , value = Some "/etc/tls/proxy/proxy.crt"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert

let mkProxySecret =
      λ(b64 : Text) →
      λ(name : Text) →
      λ(namespace : Text) →
        { apiVersion = "v1"
        , kind = "Secret"
        , metadata = { creationTimestamp = None Text, name, namespace }
        , data.`proxy.pem` = b64
        }

let DockerRegistrySecret =
      λ(secretName : Text) →
      λ(namespace : Text) →
      λ(dockerconfig_b64 : Text) →
        kubernetes.Secret::{
        , apiVersion = "v1"
        , kind = "Secret"
        , metadata = kubernetes.ObjectMeta::{
          , name = Some secretName
          , namespace = Some namespace
          }
        , type = Some "kubernetes.io/dockerconfigjson"
        , data = Some
          [ { mapKey = ".dockerconfigjson", mapValue = dockerconfig_b64 } ]
        }

-- No extra environment for polar-init. A named value, so the callers that
-- have nothing to add don't each spell out a typed empty list.
let noEnv = [] : List kubernetes.EnvVar.Type

-- What polar-init is told when its certificates come from the credentials agent (through its observer) and
-- not from cert-issuer. The pod is not told where cert-issuer is, and pairs this with an empty
-- certIssuerUrl. For a workload that is not a bootstrap identity (cert-issuer will not sign it on its token
-- alone once the floor is enforced; docs/architecture/cert-issuer-isolation.md). It verifies what it is issued
-- against cert-issuer's CA, so the pod mounts Constants.polarCaVolume and polar-init Constants.polarCaMount, and
-- seals its token only to a session offer signed by the credentials agent, `credentialsAgentIdentity`
-- (docs/architecture/core-restarts.md, "1c in detail"), which render.nu has from the catalog.
let credentialsAgentIssuerEnv =
      \(observerUrl : Text) ->
      \(credentialsAgentIdentity : Text) ->
        [ kubernetes.EnvVar::{ name = "POLAR_INIT_ISSUER", value = Some "credentials-agent" }
        , kubernetes.EnvVar::{ name = "POLAR_INIT_OBSERVER_URL", value = Some observerUrl }
        , kubernetes.EnvVar::{
          , name = "POLAR_INIT_CA_CERT"
          , value = Some "${Constants.polarCaPath}/ca.pem"
          }
        , kubernetes.EnvVar::{
          , name = "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY"
          , value = Some credentialsAgentIdentity
          }
        ]

-- The custodian fetches the escrow key only when `extraEnv` asks for it (POLAR_INIT_ESCROW_BOOTSTRAP): the
-- catalog's `encryption` capability does, for an agent that needs encryption, and no hand chart does.
-- As `makePolarInitContainer`, the client's environment given: `Constants.commonClientEnv` with a key file, or
-- `Constants.custodyClientEnv` when the key is the custody daemon's (docs/architecture/key-custody.md, step 4);
-- polar-init reads only the daemon's socket of it.
let makePolarInitContainerWith =
      \(clientEnv : List kubernetes.EnvVar.Type) ->
      \(polarInitImage : Text) ->
      \(imagePullPolicy : Text) ->
      \(saTokenVolName : Text) ->
      \(certIssuerUrl : Text) ->
      \(extraEnv : List kubernetes.EnvVar.Type) ->
      \(tokenPath : Text) ->
      \(extraMounts : List kubernetes.VolumeMount.Type) ->
      \(depCerts : List Text) ->
      \(certs : List Text) ->
        kubernetes.Container::{
        , name            = "polar-init"
        , image           = Some polarInitImage
        , imagePullPolicy = Some imagePullPolicy
        , command         = Some [ "polar-init" ]
        , terminationMessagePolicy = Some Constants.TerminationMessageFromLogs
        , securityContext = Some Constants.DropAllCapSecurityContext
        , env = Some
          (   [ kubernetes.EnvVar::{ name = "POLAR_INIT_CERT_ISSUER_URL", value = Some certIssuerUrl }
              , kubernetes.EnvVar::{ name = "POLAR_INIT_TOKEN_PATH",      value = Some tokenPath      }
              ]
            # clientEnv
            # extraEnv
          )
        , args = Some
          (   Prelude.List.concatMap Text Text (\(d : Text) -> ["--check-dep-cert", d]) depCerts
            # Prelude.List.concatMap Text Text (\(c : Text) -> ["--cert", c]) certs
          )
        , volumeMounts = Some
          (   [ kubernetes.VolumeMount::{ name = saTokenVolName, mountPath = "/workspace" } ]
            # extraMounts
          )
        }


let makePolarInitContainer = makePolarInitContainerWith Constants.commonClientEnv

-- Where a pod goes (docs/architecture/placement.md): with a placement, its pool's node, by the selector on the pool's
-- label and the toleration of its taint, both of which nix-usernetes' contract needs (a selector alone leaves the pod
-- Pending); without, nothing, and the scheduler puts it where it may.
let Placement = { key : Text, pool : Text }

let placementOf =
      \(placement : Optional Placement) ->
        { nodeSelector =
            Prelude.Optional.map
              Placement
              (List { mapKey : Text, mapValue : Text })
              (\(p : Placement) -> [ { mapKey = p.key, mapValue = p.pool } ])
              placement
        , tolerations =
            Prelude.Optional.map
              Placement
              (List kubernetes.Toleration.Type)
              ( \(p : Placement) ->
                  [ kubernetes.Toleration::{
                    , key = Some p.key
                    , operator = Some "Equal"
                    , value = Some p.pool
                    , effect = Some "NoSchedule"
                    }
                  ]
              )
              placement
        }

-- What a workload is (docs/architecture/deployments.md, step 4): a run's Job (`job` some), which the Job controller
-- replaces; or a Deployment of one, `Recreate` (never two pods of one identity), whose pod restarts in place.
let Job = { backoff_limit : Natural, ttl_after_finished_secs : Natural }

let workloadOf =
      \(w : { name : Text, namespace : Text, job : Optional Job, pod : kubernetes.PodSpec.Type }) ->
        let labels = [ { mapKey = "name", mapValue = w.name } ]

        let metadata =
              kubernetes.ObjectMeta::{
              , name = Some w.name
              , namespace = Some w.namespace
              , annotations = Some [ Constants.RejectSidecarAnnotation ]
              }

        let template =
              kubernetes.PodTemplateSpec::{
              , metadata = Some kubernetes.ObjectMeta::{
                , name = Some w.name
                , labels = Some labels
                }
              , spec = Some w.pod
              }

        in  merge
              { None =
                  kubernetes.Resource.Deployment
                    kubernetes.Deployment::{
                    , metadata
                    , spec = Some kubernetes.DeploymentSpec::{
                      , replicas = Some 1
                      , strategy = Some kubernetes.DeploymentStrategy::{
                        , type = Some "Recreate"
                        }
                      , selector = kubernetes.LabelSelector::{
                        , matchLabels = Some labels
                        }
                      , template
                      }
                    }
              , Some =
                  \(job : Job) ->
                    kubernetes.Resource.Job
                      kubernetes.Job::{
                      , metadata
                      , spec = Some kubernetes.JobSpec::{
                        , backoffLimit = Some job.backoff_limit
                        , podReplacementPolicy = Some Constants.JobPodReplacementPolicy
                        , parallelism = Some 1
                        , completions = Some 1
                        , ttlSecondsAfterFinished = Some job.ttl_after_finished_secs
                        , template
                        }
                      }
              }
              w.job

-- The workload container's restart: in a Deployment's pod, kept by the pod and restarting the whole pod on every exit; in
-- a run's Job, left to the pod (`Never`).
let restartOf =
      \(job : Optional Job) ->
        merge
          { None =
            { restartPolicy = Some "Always"
            , restartPolicyRules = Some
              [ Constants.RestartAllContainersOnEveryExit ]
            }
          , Some =
              \(_ : Job) ->
                { restartPolicy = None Text
                , restartPolicyRules = None (List kubernetes.ContainerRestartRule.Type)
                }
          }
          job

in  { makeGraphEnv
    , Placement
    , placementOf
    , Job
    , workloadOf
    , restartOf
    , makeOpaqueSecret
    , makeDeployment
    , DockerRegistrySecret
    , mkProxySecret
    , ProxyVolume
    , ProxyMount
    , ProxyEnv
    , GraphProxyEnv
    , makePolarInitContainer
    , makePolarInitContainerWith
    , noEnv
    , credentialsAgentIssuerEnv
    }
