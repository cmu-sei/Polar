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

{-
  types/build-orchestrator.dhall — build-orchestrator, the authoritative builder, and what it needs to run.

  build-orchestrator runs a repository's pipeline as a Kubernetes Job and reports what ran as provenance events. It is
  an agent like the others: kept alive by the operator, certificates from the credentials agent. The jobs it
  makes are the one place there are Jobs, and they are builds, not agents.

    stages = (Core.core coreValues).stages # build-orchestrator.buildOrchestrator buildOrchestratorValues

  The stage brings:

    the namespace jobs run in (`build-jobs`), and the Role that lets build-orchestrator make and watch them there
    and nowhere else
    the ServiceAccount jobs run under (`build-job`): one workload in the authority model, marked as having many
    instances, so every build pod has a certificate and a session of its own (docs/architecture/authority-model.md,
    "Multi-instance workloads"); and the cap on builds at once
    the object store the jobs' logs go to (the Versity S3 gateway over a volume, a `ServiceAgent`); on a cloud, an S3 bucket takes its place
    build-orchestrator's configuration (a ConfigMap), and the secret that opens the store, which the plan makes

  Which repositories are built is not here: it is the signed runtime section `mappings` (see
  docs/runbooks/git-observer.md for how a signed document is made).
-}
let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let Builder = ./builder.dhall

let P = ./plan.dhall

let Service = ./service.dhall

let Settings = ./settings.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let JSON =
      https://prelude.dhall-lang.org/JSON/package.dhall
        sha256:5f98b7722fd13509ef448b075e02b9ff98312ae7a406cf53ed25012dbc9990ac

let ns = C.polarNamespace

--| The namespace build-orchestrator's jobs run in.
let jobsNamespace = "build-jobs"

--| The annotation that marks a workload as having many instances (the fork's catalog writes it, infra/rules.nu
--| INSTANCES_ANNOTATION), which tooling reads from the rendered plan to write `instances: "many"` into the authority
--| model.
let instancesAnnotation = "polar.dev/instances"

--| The one ServiceAccount every build's job runs under. Each pod of it is an instance: its certificate names the pod.
let jobAccount = "build-job"

let storeName = "build-orchestrator-store"

let storePort = 9000

let bucket = "build-orchestrator"

let configFile = "build-orchestrator.yaml"

let configDir = "/etc/build-orchestrator"

let BuildOrchestratorValues =
      { Type =
          { platform : Builder.Platform.Type
          , images : { buildOrchestrator : Text, objectStore : Text }
          , storageClass : Optional Text
          , storeSize : Text
          -- What a job runs, when its repository's mapping gives no command of its own.
          , jobCommand : List Text
          , workspaceSizeLimit : Text
          -- What a job's pipeline container may use. `None`: the orchestrator's CI-sized defaults (2 CPU, 4Gi),
          -- which a cargo build of anything large outgrows.
          , pipelineResources :
              Optional
                { cpuLimit : Text
                , memoryLimit : Text
                , cpuRequest : Text
                , memoryRequest : Text
                }
          -- What `polar-init` is given besides what the platform says, in the jobs' pods (the platform's
          -- own `polarInitEnv`, in plain values).
          , initEnv : List Builder.JobEnv
          -- A Secret in the platform's namespace whose `ca-bundle.crt` jobs trust, for a network that
          -- re-signs TLS. The plan copies it into the jobs' namespace, and each job's git, curl and nix
          -- use it. `None`: the images' own bundles.
          , trustedCa : Optional Text
          -- The Secret the cluster's CA is kept in (the core's `caSecret`), which the plan copies the certificate of
          -- into the jobs' namespace for polar-init to verify against, when the platform pins one.
          , caSecret : Optional Text
          -- The most builds at once, which a build's pipeline makes costly (CPU, memory, disk on the node). Requests
          -- over it wait their turn. Jobs all run under one ServiceAccount (`build-job`), a workload with many
          -- instances, so nothing else limits them.
          , maxConcurrentBuilds : Natural
          -- How long a finished job (and its pod) stays in the cluster, by how it ended, in seconds. Its logs are in
          -- the object store and its provenance in the graph by then, so a success need stay only briefly; a failure
          -- longer, for someone to read what Kubernetes recorded. The orchestrator removes jobs as they pass it, and
          -- Kubernetes' own expiry on each job is the longer of the two, for when the orchestrator is not running.
          , jobRetention : { succeededSeconds : Natural, failedSeconds : Natural }
          }
      }

-- ---- The job's identity, as JSON ---------------------------------------------------------------
--
-- build-orchestrator reads the pod additions that give each job a certificate from its configuration file, as one
-- string of JSON (the loader lowercases the keys of a file's maps, which Kubernetes' camelCase names cannot
-- survive). The values are the library's (`Builder.makeJobIdentity`), so a job gets its certificate the way an
-- agent does; the pod shapes are build-orchestrator's own, written out here.
--
-- Written as text, not with the Prelude's JSON: its encoding repeats itself at every level, and a document this
-- deep takes Dhall minutes to normalise.

let quoted =
      λ(t : Text) →
        "\"${Text/replace "\"" "\\\"" (Text/replace "\\" "\\\\" t)}\""

let listOf =
      λ(items : List Text) → "[${Prelude.Text.concatSep "," items}]"

let strings = λ(items : List Text) → listOf (Prelude.List.map Text Text quoted items)

let pairs =
      λ(items : List Builder.JobEnv) →
        listOf
          ( Prelude.List.map
              Builder.JobEnv
              Text
              (λ(e : Builder.JobEnv) → "{\"name\":${quoted e.name},\"value\":${quoted e.value}}")
              items
          )

let mountJson =
      λ(name : Text) →
      λ(path : Text) →
      λ(readOnly : Bool) →
        "{\"name\":${quoted name},\"mountPath\":${quoted path},\"readOnly\":${if readOnly then "true" else "false"}}"

let identityJson =
      λ(i : Builder.JobIdentity) →
        let pullPolicy = merge { Some = λ(p : Text) → p, None = "IfNotPresent" } i.imagePullPolicy

        let uid = Natural/show i.uid

        -- The pinned CA, which polar-init verifies the job's certificate against: a ConfigMap in the jobs' namespace.
        let caMount =
              merge
                { Some =
                    λ(ca : { configMap : Text, volume : Text, dir : Text }) →
                      [ mountJson ca.volume ca.dir True ]
                , None = [] : List Text
                }
                i.pinnedCa

        let caVolume =
              merge
                { Some =
                    λ(ca : { configMap : Text, volume : Text, dir : Text }) →
                      [ "{\"name\":${quoted ca.volume},\"configMap\":{\"name\":${quoted ca.configMap}}}" ]
                , None = [] : List Text
                }
                i.pinnedCa

        let polarInit =
              "{\"name\":\"polar-init\",\"image\":${quoted i.image},\"imagePullPolicy\":${quoted pullPolicy},"
              ++ "\"command\":${strings [ "polar-init" ]},\"args\":${strings i.args},\"env\":${pairs i.env},"
              ++ "\"volumeMounts\":${listOf ([ mountJson i.tokenVolume i.tokenDir True, mountJson i.certVolume i.certDir False ] # caMount)},"
              ++ "\"securityContext\":{\"runAsUser\":${uid},\"runAsGroup\":${uid},\"runAsNonRoot\":true,\"allowPrivilegeEscalation\":false,\"capabilities\":{\"drop\":[\"ALL\"]}}}"

        let tokenVolume =
              "{\"name\":${quoted i.tokenVolume},\"projected\":{\"sources\":[{\"serviceAccountToken\":{\"audience\":${quoted i.tokenAudience},\"expirationSeconds\":${Natural/show i.tokenExpirySecs},\"path\":\"token\"}}]}}"

        let certVolume = "{\"name\":${quoted i.certVolume},\"emptyDir\":{}}"

        in  "{\"initContainers\":[${polarInit}],\"volumes\":${listOf ([ tokenVolume, certVolume ] # caVolume)},\"pipelineMounts\":[${mountJson i.certVolume i.certDir True}],\"pipelineEnv\":${pairs i.pipelineEnv}}"

let envVar =
      λ(name : Text) →
      λ(value : Text) →
        JSON.object (toMap { name = JSON.string name, value = JSON.string value })

--| The configuration file, as JSON (YAML reads it). The store's keys are not in it: they come in as
--| variables from the Secret, which the loader reads over the file.
let configuration =
      λ(v : BuildOrchestratorValues.Type) →
        JSON.render
          ( JSON.object
              ( toMap
                  { max_concurrent_builds = JSON.natural v.maxConcurrentBuilds
                  , backend =
                      JSON.object
                        ( toMap
                            { driver = JSON.string "kubernetes"
                            , command = JSON.array (Prelude.List.map Text JSON.Type JSON.string v.jobCommand)
                            -- What a job needs to reach the broker; its certificate is its own.
                            , job_env =
                                JSON.array
                                  [ envVar "BROKER_ADDR" v.platform.cassiniAddr
                                  , envVar "CASSINI_SERVER_NAME" v.platform.cassiniServerName
                                  ]
                            , kubernetes =
                                JSON.object
                                  ( toMap
                                      { namespace = JSON.string jobsNamespace
                                      , workspace_size_limit = JSON.string v.workspaceSizeLimit
                                      -- The pipeline records which commit and tree it built, so it needs the repository.
                                      , keep_git = JSON.bool True
                                      , retention =
                                          JSON.object
                                            ( toMap
                                                { succeeded_secs = JSON.natural v.jobRetention.succeededSeconds
                                                , failed_secs = JSON.natural v.jobRetention.failedSeconds
                                                }
                                            )
                                      , resources =
                                          merge
                                            { Some =
                                                λ ( r
                                                  : { cpuLimit : Text
                                                    , memoryLimit : Text
                                                    , cpuRequest : Text
                                                    , memoryRequest : Text
                                                    }
                                                  ) →
                                                  JSON.object
                                                    ( toMap
                                                        { cpu_limit = JSON.string r.cpuLimit
                                                        , memory_limit = JSON.string r.memoryLimit
                                                        , cpu_request = JSON.string r.cpuRequest
                                                        , memory_request = JSON.string r.memoryRequest
                                                        }
                                                    )
                                            , None = JSON.null
                                            }
                                            v.pipelineResources
                                      , ca_bundle_secret =
                                          merge
                                            { Some = λ(name : Text) → JSON.string name
                                            , None = JSON.null
                                            }
                                            v.trustedCa
                                      , image_pull_policy = JSON.string (merge { Some = λ(p : Text) → p, None = "IfNotPresent" } v.platform.imagePullPolicy)
                                      -- A job's certificate: `polar-init`, no secret shared between builds, issued to
                                      -- the ServiceAccount the job runs under (`account`), whose pods are instances of
                                      -- one workload, each with a certificate that names its pod.
                                      , identity = JSON.string (identityJson (Builder.makeJobIdentity v.platform v.initEnv))
                                      , account = JSON.string jobAccount
                                      }
                                  )
                            }
                        )
                  , storage =
                      JSON.object
                        ( toMap
                            { endpoint_url =
                                JSON.string "http://${storeName}.${ns}.svc.cluster.local:${Natural/show storePort}"
                            , bucket = JSON.string bucket
                            , region = JSON.string "us-east-1"
                            }
                        )
                  }
              )
          )

let rule =
      λ(apiGroups : List Text) →
      λ(resources : List Text) →
      λ(verbs : List Text) →
        kubernetes.PolicyRule::{
        , apiGroups = Some apiGroups
        , resources = Some resources
        , verbs
        }

let buildOrchestrator
    : BuildOrchestratorValues.Type → List P.Stage.Type
    = λ(v : BuildOrchestratorValues.Type) →
        let metadata =
              λ(namespace : Text) →
              λ(name : Text) →
                kubernetes.ObjectMeta::{ name = Some name, namespace = Some namespace }

        let agent =
              Builder.BuilderAgent::{
              , name = "build-orchestrator"
              , namespace = ns
              , image = v.images.buildOrchestrator
              , provides = [ "cassini", "kube-api-read", "encryption" ]
              -- It opens the credentials a mapping names, sealed to its escrow key: so it needs the custody daemon.
              , encrypts = True
              , shutdownBudgetSecs = 30
              , serviceAccountToken = True
              , configFiles = [ { configMap = "build-orchestrator-config", mountPath = configDir } ]
              , settings =
                [ Settings.setting "ORCHESTRATOR_CONFIG_FILE" "${configDir}/${configFile}"
                , Settings.secretSetting "BUILD_ORCHESTRATOR__STORAGE__ACCESS_KEY" "build-orchestrator-store-keys" "access_key"
                , Settings.secretSetting "BUILD_ORCHESTRATOR__STORAGE__SECRET_KEY" "build-orchestrator-store-keys" "secret_key"
                ]
              }

        let store =
              Service.ServiceAgent::{
              , name = storeName
              , namespace = ns
              , image = v.images.objectStore
              , imagePullPolicy = v.platform.imagePullPolicy
              , args = Some [ "--port", ":${Natural/show storePort}", "posix", "--sidecar", "/data/sidecar", "/data/buckets" ]
              , env =
                [ Settings.secretSetting "ROOT_ACCESS_KEY" "build-orchestrator-store-keys" "access_key"
                , Settings.secretSetting "ROOT_SECRET_KEY" "build-orchestrator-store-keys" "secret_key"
                ]
              , port = Some storePort
              , volumes =
                [ kubernetes.Volume::{
                  , name = "data"
                  , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                    , claimName = "${storeName}-data"
                    }
                  }
                ]
              , mounts = [ kubernetes.VolumeMount::{ name = "data", mountPath = "/data" } ]
              , fsGroup = Some 1000
              , probe = Some kubernetes.Probe::{
                , tcpSocket = Some kubernetes.TCPSocketAction::{
                  , port = kubernetes.NatOrString.Nat storePort
                  }
                , initialDelaySeconds = Some 10
                , periodSeconds = Some 10
                }
              , readiness = Some kubernetes.Probe::{
                , tcpSocket = Some kubernetes.TCPSocketAction::{
                  , port = kubernetes.NatOrString.Nat storePort
                  }
                , periodSeconds = Some 3
                }
              }

        let jobServiceAccount =
              kubernetes.Resource.ServiceAccount
                kubernetes.ServiceAccount::{
                , metadata = kubernetes.ObjectMeta::{
                  , name = Some jobAccount
                  , namespace = Some jobsNamespace
                  , annotations = Some [ { mapKey = instancesAnnotation, mapValue = "many" } ]
                  }
                -- The only token a job's pod has is the one `polar-init` is given, projected.
                , automountServiceAccountToken = Some False
                }

        in  [ P.Stage::{
              , name = "build-orchestrator"
              , description = Some "build-orchestrator, the authoritative builder, its object store, and the namespace its jobs run in"
              , shared = True
              , secrets =
                [ P.Secret::{
                  , namespace = ns
                  , name = "build-orchestrator-store-keys"
                  , keys =
                    [ { mapKey = "access_key", mapValue = P.Value.Literal { literal = "build-orchestrator" } }
                    , { mapKey = "secret_key"
                      , mapValue = P.Value.Random { random = 24, id = None Text }
                      }
                    ]
                  }
                ]
                # merge
                    { Some =
                        λ(name : Text) →
                          [ P.Secret::{
                            , namespace = jobsNamespace
                            , name
                            , keys =
                              [ { mapKey = "ca-bundle.crt"
                                , mapValue = P.Value.Copy { copy = { namespace = ns, name, key = "ca-bundle.crt" } }
                                }
                              ]
                            }
                          ]
                    , None = [] : List P.Secret.Type
                    }
                    v.trustedCa
              , configMaps =
                  merge
                    { Some =
                        λ(pinned : Text) →
                          merge
                            { Some =
                                λ(secret : Text) →
                                  [ P.pinnedCa
                                      { namespace = jobsNamespace
                                      , name = pinned
                                      , secret = { namespace = ns, name = secret }
                                      }
                                  ]
                            , None = [] : List P.ConfigMap.Type
                            }
                            v.caSecret
                    , None = [] : List P.ConfigMap.Type
                    }
                    v.platform.pinnedCa
              , objects =
                    P.k8s
                      ( [ kubernetes.Resource.Namespace
                          kubernetes.Namespace::{
                          , metadata = kubernetes.ObjectMeta::{ name = Some jobsNamespace }
                          }
                      , kubernetes.Resource.ServiceAccount (Builder.makeServiceAccount ns "build-orchestrator")
                      -- Make and watch jobs in the jobs' namespace, and read their pods and logs. Nothing
                      -- else, and nowhere else.
                      , kubernetes.Resource.Role
                          kubernetes.Role::{
                          , metadata = metadata jobsNamespace "build-orchestrator"
                          , rules = Some
                            [ rule [ "batch" ] [ "jobs" ] [ "create", "get", "list", "watch", "delete" ]
                            , rule [ "" ] [ "pods", "pods/log" ] [ "get", "list", "watch" ]
                            -- One Secret per build that clones a private repository, for the credential: made
                            -- before the job, given to it so it goes with it, deleted if the job cannot be made.
                            -- It can put a Secret there and cannot read one back.
                            , rule [ "" ] [ "secrets" ] [ "create", "patch", "delete" ]
                            ]
                          }
                      , kubernetes.Resource.RoleBinding
                          kubernetes.RoleBinding::{
                          , metadata = metadata jobsNamespace "build-orchestrator"
                          , roleRef =
                            { apiGroup = "rbac.authorization.k8s.io"
                            , kind = "Role"
                            , name = "build-orchestrator"
                            }
                          , subjects = Some
                            [ kubernetes.Subject::{ kind = "ServiceAccount", name = "build-orchestrator", namespace = Some ns } ]
                          }
                      , kubernetes.Resource.ConfigMap
                          kubernetes.ConfigMap::{
                          , metadata = metadata ns "build-orchestrator-config"
                          , data = Some [ { mapKey = configFile, mapValue = configuration v } ]
                          }
                      , kubernetes.Resource.PersistentVolumeClaim
                          (Builder.makePvc ns "${storeName}-data" v.storeSize v.storageClass)
                      , kubernetes.Resource.Service (Builder.makeService ns storeName storePort)
                      ]
                      # [ jobServiceAccount ]
                      )
                  # P.agents
                      [ Service.makeServiceAgent store
                      , Builder.makePolarAgent v.platform agent
                      ]
              , waits = [ P.waitForAgent ns storeName, P.waitForAgent ns "build-orchestrator" ]
              }
            ]

in  { BuildOrchestratorValues, jobsNamespace, instancesAnnotation, jobAccount, buildOrchestrator }
