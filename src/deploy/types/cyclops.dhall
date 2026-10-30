{-
  types/cyclops.dhall — Cyclops, the authoritative builder, and what it needs to run.

  Cyclops runs a repository's pipeline as a Kubernetes Job and reports what ran as provenance events. It is
  an agent like the others: kept alive by the operator, certificates from the credentials agent. The jobs it
  makes are the one place there are Jobs, and they are builds, not agents.

    stages = (Core.core coreValues).stages # Cyclops.cyclops cyclopsValues

  The stage brings:

    the namespace jobs run in (`cyclops-jobs`), and the Role that lets Cyclops make and watch them there
    and nowhere else
    the object store the jobs' logs go to (the Versity S3 gateway over a volume, a `ServiceAgent`); on a cloud, an S3 bucket takes its place
    Cyclops' configuration (a ConfigMap), and the secret that opens the store, which the plan makes

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

--| The namespace Cyclops' jobs run in.
let jobsNamespace = "cyclops-jobs"

let storeName = "cyclops-store"

let storePort = 9000

let bucket = "cyclops"

let configFile = "cyclops.yaml"

let configDir = "/etc/cyclops"

let CyclopsValues =
      { Type =
          { platform : Builder.Platform.Type
          , images : { cyclops : Text, objectStore : Text }
          , storageClass : Optional Text
          , storeSize : Text
          -- What a job runs, when its repository's mapping gives no command of its own.
          , jobCommand : List Text
          , workspaceSizeLimit : Text
          }
      }

let envVar =
      λ(name : Text) →
      λ(value : Text) →
        JSON.object (toMap { name = JSON.string name, value = JSON.string value })

--| The configuration file, as JSON (YAML reads it). The store's keys are not in it: they come in as
--| variables from the Secret, which the loader reads over the file.
let configuration =
      λ(v : CyclopsValues.Type) →
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

let cyclops
    : CyclopsValues.Type → List P.Stage.Type
    = λ(v : CyclopsValues.Type) →
        let metadata =
              λ(namespace : Text) →
              λ(name : Text) →
                kubernetes.ObjectMeta::{ name = Some name, namespace = Some namespace }

        let agent =
              Builder.BuilderAgent::{
              , name = "cyclops"
              , namespace = ns
              , image = v.images.cyclops
              , provides = [ "cassini", "kube-api-read" ]
              , shutdownBudgetSecs = 30
              , serviceAccountToken = True
              , configFiles = [ { configMap = "cyclops-config", mountPath = configDir } ]
              , settings =
                [ Settings.setting "ORCHESTRATOR_CONFIG_FILE" "${configDir}/${configFile}"
                , Settings.secretSetting "CYCLOPS__STORAGE__ACCESS_KEY" "cyclops-store-keys" "access_key"
                , Settings.secretSetting "CYCLOPS__STORAGE__SECRET_KEY" "cyclops-store-keys" "secret_key"
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
                [ Settings.secretSetting "ROOT_ACCESS_KEY" "cyclops-store-keys" "access_key"
                , Settings.secretSetting "ROOT_SECRET_KEY" "cyclops-store-keys" "secret_key"
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
              , name = "cyclops"
              , description = Some "Cyclops, the authoritative builder, its object store, and the namespace its jobs run in"
              , shared = True
              , secrets =
                [ P.Secret::{
                  , namespace = ns
                  , name = "cyclops-store-keys"
                  , keys =
                    [ { mapKey = "access_key", mapValue = P.Value.Literal { literal = "cyclops" } }
                    , { mapKey = "secret_key"
                      , mapValue = P.Value.Random { random = 24, id = None Text }
                      }
                    ]
                  }
                ]
              , objects =
                    P.k8s
                      [ kubernetes.Resource.Namespace
                          kubernetes.Namespace::{
                          , metadata = kubernetes.ObjectMeta::{ name = Some jobsNamespace }
                          }
                      , kubernetes.Resource.ServiceAccount (Builder.makeServiceAccount ns "cyclops")
                      -- Make and watch jobs in the jobs' namespace, and read their pods and logs. Nothing
                      -- else, and nowhere else.
                      , kubernetes.Resource.Role
                          kubernetes.Role::{
                          , metadata = metadata jobsNamespace "cyclops"
                          , rules = Some
                            [ rule [ "batch" ] [ "jobs" ] [ "create", "get", "list", "watch", "delete" ]
                            , rule [ "" ] [ "pods", "pods/log" ] [ "get", "list", "watch" ]
                            ]
                          }
                      , kubernetes.Resource.RoleBinding
                          kubernetes.RoleBinding::{
                          , metadata = metadata jobsNamespace "cyclops"
                          , roleRef =
                            { apiGroup = "rbac.authorization.k8s.io"
                            , kind = "Role"
                            , name = "cyclops"
                            }
                          , subjects = Some
                            [ kubernetes.Subject::{ kind = "ServiceAccount", name = "cyclops", namespace = Some ns } ]
                          }
                      , kubernetes.Resource.ConfigMap
                          kubernetes.ConfigMap::{
                          , metadata = metadata ns "cyclops-config"
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
              , waits = [ P.waitForAgent ns storeName, P.waitForAgent ns "cyclops" ]
              }
            ]

in  { CyclopsValues, jobsNamespace, cyclops }
