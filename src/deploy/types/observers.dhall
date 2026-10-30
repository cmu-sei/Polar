{-
  types/observers.dhall — the agents that watch a delivery pipeline and write what they see to the graph.

  What the graph says about a software delivery pipeline comes from these: the Kubernetes observer reads
  the cluster and the consumer records it. They belong with the core and follow it, as stages of the
  same shape; an application plan includes the ones it wants after `Core.core values`.

    stages = (Core.core coreValues).stages # Observers.kubernetes observerValues

  Like the core's, these stages are `shared`: another application on the same cluster asks for the same
  agents, and they are applied once.
-}
let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let Builder = ./builder.dhall

let P = ./plan.dhall

let Settings = ./settings.dhall

let ns = C.polarNamespace

--| What a deployer states about the observers: the images, and the platform they are told about
--| (`Core.core values .platform`).
let ObserverValues =
      { Type =
          { platform : Builder.Platform.Type
          , images :
              { kubeObserver : Text
              , kubeConsumer : Text
              , gitObserver : Text
              , gitProcessor : Text
              , gitController : Text
              , buildProcessor : Text
              }
          -- A Secret in the observers' namespace whose `ca-bundle.crt` the git observer trusts, for a
          -- network that re-signs TLS (see `BuilderAgent.trustedCa`). `None`: the image's own bundle.
          , trustedCa : Optional Text
          }
      }

let rule =
      λ(apiGroups : List Text) →
      λ(resources : List Text) →
        kubernetes.PolicyRule::{
        , apiGroups = Some apiGroups
        , resources = Some resources
        , verbs = [ "get", "list", "watch" ]
        }

--| What the Kubernetes observer watches, and nothing else: read, never write.
let kubeObserverRules =
      [ rule [ "" ] [ "namespaces", "nodes", "pods", "configmaps" ]
      , rule [ "apps" ] [ "deployments", "replicasets" ]
      , rule [ "batch" ] [ "jobs" ]
      , rule [ "apiextensions.k8s.io" ] [ "customresourcedefinitions" ]
      , rule [ "rbac.authorization.k8s.io" ] [ "clusterroles", "clusterrolebindings" ]
      , rule [ "kustomize.toolkit.fluxcd.io" ] [ "kustomizations" ]
      , rule [ "source.toolkit.fluxcd.io" ] [ "ocirepositories" ]
      ]

--| The Kubernetes observer and its consumer: the first reads the cluster and publishes what it sees, the
--| second records it in the graph.
let kubeStages =
      λ(v : ObserverValues.Type) →
        let metadata =
              λ(name : Text) →
                kubernetes.ObjectMeta::{ name = Some name, namespace = Some ns }

        let observer =
              Builder.BuilderAgent::{
              , name = "kube-observer"
              , namespace = ns
              , image = v.images.kubeObserver
              , provides = [ "cassini", "kube-api-read" ]
              , shutdownBudgetSecs = 45
              , serviceAccountToken = True
              }

        let consumer =
              Builder.BuilderAgent::{
              , name = "kube-consumer"
              , namespace = ns
              , image = v.images.kubeConsumer
              , provides = [ "cassini", "graph-write", "encryption" ]
              , encrypts = True
              , shutdownBudgetSecs = 50
              , settings = Settings.graphSettings ns
              }

        let clusterMetadata = kubernetes.ObjectMeta::{ name = Some "kube-observer-read" }

        in  [ P.Stage::{
              , name = "kubernetes"
              , description = Some "the Kubernetes observer, which reads the cluster, and the consumer that records it"
              , shared = True
              , objects =
                    P.k8s
                      [ kubernetes.Resource.ServiceAccount
                          (Builder.makeServiceAccount ns "kube-observer")
                      , kubernetes.Resource.ServiceAccount
                          (Builder.makeServiceAccount ns "kube-consumer")
                      , kubernetes.Resource.ClusterRole
                          kubernetes.ClusterRole::{
                          , metadata = clusterMetadata
                          , rules = Some kubeObserverRules
                          }
                      , kubernetes.Resource.ClusterRoleBinding
                          kubernetes.ClusterRoleBinding::{
                          , metadata = clusterMetadata
                          , roleRef =
                            { apiGroup = "rbac.authorization.k8s.io"
                            , kind = "ClusterRole"
                            , name = "kube-observer-read"
                            }
                          , subjects = Some
                            [ kubernetes.Subject::{
                              , kind = "ServiceAccount"
                              , name = "kube-observer"
                              , namespace = Some ns
                              }
                            ]
                          }
                      ]
                  # P.agents
                      [ Builder.makePolarAgent v.platform consumer
                      , Builder.makePolarAgent v.platform observer
                      ]
              , waits =
                [ P.waitForAgent ns "kube-consumer", P.waitForAgent ns "kube-observer" ]
              }
            ]

--| The git agents: the observer clones and fetches the repositories it is told to watch and publishes
--| what it finds, the processor writes that to the graph, and the controller decides which repositories
--| are watched, from the runtime documents the scheduler carries (nothing is watched until a signed
--| `repositories` document names one).
let gitStages =
      λ(v : ObserverValues.Type) →
        let account =
              λ(name : Text) →
                kubernetes.Resource.ServiceAccount (Builder.makeServiceAccount ns name)

        let observer =
              Builder.BuilderAgent::{
              , name = "git-repo-observer"
              , namespace = ns
              , image = v.images.gitObserver
              , provides = [ "cassini", "encryption" ]
              , encrypts = True
              , shutdownBudgetSecs = 45
              -- The clones can be fetched again, so they do not outlive the pod.
              , scratch = [ { name = "cache", mountPath = "/var/lib/git-repo-observer" } ]
              , settings = [ Settings.setting "POLAR_CACHE_ROOT" "/var/lib/git-repo-observer" ]
              , trustedCa = v.trustedCa
              }

        let processor =
              Builder.BuilderAgent::{
              , name = "git-repo-processor"
              , namespace = ns
              , image = v.images.gitProcessor
              , provides = [ "cassini", "graph-write" ]
              , shutdownBudgetSecs = 50
              , settings = Settings.graphSettings ns
              }

        let controller =
              Builder.BuilderAgent::{
              , name = "git-repo-controller"
              , namespace = ns
              , image = v.images.gitController
              , shutdownBudgetSecs = 20
              }

        in  [ P.Stage::{
              , name = "git-agents"
              , description = Some "the git observer, processor and controller: watch the repositories the scheduler's documents name"
              , shared = True
              , objects =
                    P.k8s [ account "git-repo-observer", account "git-repo-processor", account "git-repo-controller" ]
                  # P.agents
                      [ Builder.makePolarAgent v.platform processor
                      , Builder.makePolarAgent v.platform controller
                      , Builder.makePolarAgent v.platform observer
                      ]
              , waits =
                [ P.waitForAgent ns "git-repo-processor"
                , P.waitForAgent ns "git-repo-controller"
                , P.waitForAgent ns "git-repo-observer"
                ]
              }
            ]

--| The build processor: it reads the provenance events that builds publish (build-orchestrator's own, and the
--| pipeline library's from a job) and writes them to the graph.
let buildStages =
      λ(v : ObserverValues.Type) →
        let processor =
              Builder.BuilderAgent::{
              , name = "build-processor"
              , namespace = ns
              , image = v.images.buildProcessor
              , provides = [ "cassini", "graph-write" ]
              , shutdownBudgetSecs = 50
              , settings = Settings.graphSettings ns
              }

        in  [ P.Stage::{
              , name = "build-processor"
              , description = Some "the build processor: provenance events from builds, written to the graph"
              , shared = True
              , objects =
                    P.k8s [ kubernetes.Resource.ServiceAccount (Builder.makeServiceAccount ns "build-processor") ]
                  # P.agents [ Builder.makePolarAgent v.platform processor ]
              , waits = [ P.waitForAgent ns "build-processor" ]
              }
            ]

in  { ObserverValues, kubernetes = kubeStages, git = gitStages, build = buildStages }
