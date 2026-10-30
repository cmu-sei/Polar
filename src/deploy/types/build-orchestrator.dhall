{-
  types/build-orchestrator.dhall — build-orchestrator, the authoritative builder, and what it needs to run.

  build-orchestrator runs a repository's pipeline as a Kubernetes Job and reports what ran as provenance events. It is
  an agent like the others: kept alive by the operator, certificates from the credentials agent. The jobs it
  makes are the one place there are Jobs, and they are builds, not agents.

    stages = (Core.core coreValues).stages # build-orchestrator.buildOrchestrator buildOrchestratorValues

  The stage brings:

    the namespace jobs run in (`build-jobs`), and the Role that lets build-orchestrator make and watch them there
    and nowhere else
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
          -- What `polar-init` is given besides what the platform says, in the jobs' pods (the platform's
          -- own `polarInitEnv`, in plain values).
          , initEnv : List Builder.JobEnv
          -- A Secret in the platform's namespace whose `ca-bundle.crt` jobs trust, for a network that
          -- re-signs TLS. The plan copies it into the jobs' namespace, and each job's git, curl and nix
          -- use it. `None`: the images' own bundles.
          , trustedCa : Optional Text
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

        let polarInit =
              "{\"name\":\"polar-init\",\"image\":${quoted i.image},\"imagePullPolicy\":${quoted pullPolicy},"
              ++ "\"command\":${strings [ "polar-init" ]},\"args\":${strings i.args},\"env\":${pairs i.env},"
              ++ "\"volumeMounts\":${listOf [ mountJson i.tokenVolume i.tokenDir True, mountJson i.certVolume i.certDir False ]},"
              ++ "\"securityContext\":{\"runAsUser\":${uid},\"runAsGroup\":${uid},\"runAsNonRoot\":true,\"allowPrivilegeEscalation\":false,\"capabilities\":{\"drop\":[\"ALL\"]}}}"

        let tokenVolume =
              "{\"name\":${quoted i.tokenVolume},\"projected\":{\"sources\":[{\"serviceAccountToken\":{\"audience\":${quoted i.tokenAudience},\"expirationSeconds\":${Natural/show i.tokenExpirySecs},\"path\":\"token\"}}]}}"

        let certVolume = "{\"name\":${quoted i.certVolume},\"emptyDir\":{}}"

        in  "{\"initContainers\":[${polarInit}],\"volumes\":[${tokenVolume},${certVolume}],\"pipelineMounts\":[${mountJson i.certVolume i.certDir True}],\"pipelineEnv\":${pairs i.pipelineEnv}}"

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
                  { backend =
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
                                      , ca_bundle_secret =
                                          merge
                                            { Some = λ(name : Text) → JSON.string name
                                            , None = JSON.null
                                            }
                                            v.trustedCa
                                      , image_pull_policy = JSON.string (merge { Some = λ(p : Text) → p, None = "IfNotPresent" } v.platform.imagePullPolicy)
                                      -- A job's certificate: its own ServiceAccount, `polar-init`, no secret
                                      -- shared between builds.
                                      , identity = JSON.string (identityJson (Builder.makeJobIdentity v.platform v.initEnv))
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
              , provides = [ "cassini", "kube-api-read" ]
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
              , objects =
                    P.k8s
                      [ kubernetes.Resource.Namespace
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
                            -- One ServiceAccount per build: the identity on the job's certificate. It is
                            -- owned by the job, so it goes with it.
                            , rule [ "" ] [ "serviceaccounts" ] [ "create", "get", "patch", "delete" ]
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
                  # P.agents
                      [ Service.makeServiceAgent store
                      , Builder.makePolarAgent v.platform agent
                      ]
              , waits = [ P.waitForAgent ns storeName, P.waitForAgent ns "build-orchestrator" ]
              }
            ]

in  { BuildOrchestratorValues, jobsNamespace, buildOrchestrator }
