{-
  values.dhall — local development deployment values

  This file defines the concrete values for a local development deployment.
  It replaces the previous cert-manager-based TLS model with the cert-issuer
  init container model. Key changes from the previous version:

  - All cert-manager TLS fields removed. Certs are now issued at pod startup
    by cert-issuer-init using projected SA tokens.
  - Every agent now carries certClient = Constants.defaultCertClientConfig.
    Override cert_type to "server" for Cassini only.
  - WithServiceAccount is now only for agents that need Kubernetes API access
    (kubeObserver, buildOrchestrator). It is no longer used for TLS.
  - commonClientTls and commonClientEnv now point at certDir paths written
    by cert-issuer-init rather than cert-manager secret mounts.
  - cassiniServerName is now the normalized SA identity DNS form rather than
    the old cert-manager CN "polar".

  Environment variables required at render time:
    CI_COMMIT_SHORT_SHA  — image tag (defaults to "latest")
    NAMESPACE            — Kubernetes namespace (defaults to "polar")
    GRAPH_NAMESPACE      — Neo4j namespace (defaults to "polar-db")
    GITLAB_TOKEN         — GitLab API token
    GRAPH_PASSWORD       — Neo4j password
    DOCKER_AUTH_JSON     — OCI registry auth JSON
    NEO4J_TLS_CA_CERT_CONTENT — Neo4j TLS CA cert content
-}
let kubernetes = ../types/kubernetes.dhall

let Constants = ../types/constants.dhall

let Cassini = ../types/cassini.dhall

let Agents = ../types/agents.dhall

let functions = ../types/functions.dhall

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let imagePullPolicy = "Never"

let img = \(name : Text) -> "${name}:${commitSha}"

let tls = Constants.commonClientTls

let imagePullSecrets =
      [ kubernetes.LocalObjectReference::{
        , name = Some Constants.imagePullSecretName
        }
      ]

let certIssuer
    : Agents.CertIssuer
    = { name = "cert-issuer"
      , image = img "cert-issuer"
      , serviceAccountName = "cert-issuer-sa"
      , config = ./conf/issuer-config.json as Text
      }

let cassini
    : Cassini.Type
    =     Cassini.defaults
      //  { name = "cassini"
          , image = img "cassini"
          , serviceAccountName = "cassini"
          , certClient =
              Constants.defaultCertClientConfig // { cert_type = "server" }
          }

let neo4j =
      { name = "polar-neo4j"
      , hostName = "neo4j"
      , namespace = Constants.GraphNamespace
      , image = "neo4j:5.26.2"
      , configVolume = "neo4j-config-copy"
      , certificatesVolume = "neo4j-certificates"
      , confDir = "/var/lib/neo4j/conf"
      , certDir = "/var/lib/neo4j/certificates"
      , ports = { http = 7474, https = 7473, bolt = 7687 }
      , config = { name = "neo4j-config", path = "/var/lib/neo4j/conf" }
      , enableTls = True
      , volumes =
        { data =
          { name = "polar-db-data"
          , storageClassName = Some "managed-csi"
          , storageSize = "10Gi"
          , mountPath = "/var/lib/neo4j/data"
          }
        , logs =
          { name = "polar-db-logs"
          , storageClassName = Some "managed-csi"
          , storageSize = "10Gi"
          , mountPath = "/var/lib/neo4j/logs"
          }
        , certs =
          { name = "polar-db-certs"
          , storageClassName = Some "managed-csi"
          , storageSize = "1Gi"
          , mountPath = "/var/lib/neo4j/certificates"
          }
        }
      }

let neo4jDNSName =
      "${Constants.neo4jServiceName}.${Constants.GraphNamespace}.svc.cluster.local"

let neo4jBoltAddr = "bolt+s://${neo4jDNSName}:${Natural/show neo4j.ports.bolt}"

let jaeger =
      { name = "jaeger"
      , image = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
      , ports = { http = 16686, grpc = 14250, thrift = 6831 }
      , service.name = "jaeger-svc"
      }

let jaegerDNSName =
      "http://${jaeger.service.name}.${Constants.PolarNamespace}.svc.cluster.local:${Natural/show
                                                                                       jaeger.ports.http}/v1/traces"

let gitlabObserver
    : Agents.GitlabObserver
    = { name = "polar-gitlab-observer"
      , image = img "polar-gitlab-observer"
      , endpoint = "https://gitlab.sandbox.labz.s-box.org"
      , maxBackoffSecs = 300
      , baseIntervalSecs = 30
      , token = Some env:GITLAB_TOKEN as Text
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let gitlabConsumer
    : Agents.GitlabConsumer
    = { name = "polar-gitlab-consumer"
      , image = img "polar-gitlab-consumer"
      , graph = Constants.graphConfig
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let linker
    : Agents.ProvenanceLinker
    = { name = Constants.ProvenanceLinkerName
      , image = img "polar-linker-agent"
      , graph = Constants.graphConfig
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let resolver
    : Agents.ProvenanceResolver
    = { name = Constants.ProvenanceResolverName
      , image = img "provenance-resolver-agent"
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let gitObserver
    : Agents.GitObserver
    = { name = "git-repo-observer"
      , image = img "git-observer"
      , config = ./conf/git.json as Text
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let gitConsumer
    : Agents.GitConsumer
    = { name = "git-repo-consumer"
      , image = img "git-processor"
      , graph = Constants.graphConfig
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let gitScheduler
    : Agents.GitScheduler
    = { name = "git-scheduler"
      , image = img "git-processor"
      , graph = Constants.graphConfig
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let kubeObserver
    : Agents.KubeObserver
    = { name = "kube-observer"
      , image = img "kube-observer"
      , serviceAccountName = "kube-observer-sa"
      , secretName = "kube-observer-sa-token"
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let kubeConsumer
    : Agents.KubeConsumer
    = { name = "kube-consumer"
      , image = img "kube-consumer"
      , graph = Constants.graphConfig
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let buildOrchestrator
    : Agents.BuildOrchestrator
    = { name = "build-orchestrator"
      , image = img "build-orchestrator"
      , serviceAccountName = "build-orchestrator-sa"
      , secretName = "build-orchestrator-sa-token"
      , config = ./conf/cyclops.yaml as Text
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let buildProcessor
    : Agents.BuildProcessor
    = { name = "build-processor"
      , image = img "polar-build-processor"
      , graph = Constants.graphConfig
      , certClient = Constants.defaultCertClientConfig
      , tls
      }

let proxyCACert = None Text

in  { imagePullSecrets
    , imagePullPolicy
    , certIssuer
    , cassini
    , gitlabObserver
    , gitlabConsumer
    , kubeObserver
    , kubeConsumer
    , linker
    , resolver
    , gitObserver
    , gitConsumer
    , gitScheduler
    , neo4j
    , neo4jDNSName
    , neo4jBoltAddr
    , jaeger
    , jaegerDNSName
    , buildProcessor
    , buildOrchestrator
    , proxyCACert
    , isLocal = True
    }
