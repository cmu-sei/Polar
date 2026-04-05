let Constants = ../types/constants.dhall

let kubernetes = ../types/kubernetes.dhall

let Cassini = ../types/cassini.dhall

let Agents = ../types/agents.dhall

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let imagePullPolicy = "IfNotPresent"

let img = \(name : Text) -> "polar/${name}:${commitSha}"

let tls = Constants.commonClientTls

-- =============================================================================
-- Infrastructure
-- =============================================================================

let imagePullSecrets =
      [ kubernetes.LocalObjectReference::{ name = Some Constants.imagePullSecretName } ]

let cassiniTlsVolumeMount =
      kubernetes.VolumeMount::{ name = "client-tls", mountPath = Constants.tlsPath }

let cassini
    : Cassini
    = { name = "cassini"
      , image = img "cassini"
      , ports = { http = 3000, tcp = 8080 }
      , tls =
        { certificateRequestName = "cassini-certificate"
        , certificateSpec =
          { commonName = Constants.mtls.commonName
          , dnsNames = [ "cassini-ip-svc.polar.svc.cluster.local" ]
          , duration = "2160h"
          , issuerRef = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
          , renewBefore = "360h"
          , secretName = Constants.CassiniServerCertificateSecret
          }
        }
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
      , enableTls = True   -- or false if you want HTTP mode
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

let neo4jBoltAddr = "neo4j://${neo4jDNSName}:${Natural/show neo4j.ports.bolt}"

let jaeger =
      { name = "jaeger"
      , image = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
      , ports = { http = 16686, grpc = 14250, thrift = 6831 }
      , service.name = "jaeger-svc"
      }

let jaegerDNSName =
      "http://${jaeger.service.name}.${Constants.PolarNamespace}.svc.cluster.local:${Natural/show jaeger.ports.http}/v1/traces"

-- =============================================================================
-- Agents
-- =============================================================================

let gitlabObserver
    : Agents.GitlabObserver
    = { name = "polar-gitlab-observer"
      , image = img "polar-gitlab-observer"
      , endpoint = "https://gitlab.sandbox.labz.s-box.org"
      , maxBackoffSecs = 300
      , baseIntervalSecs = 30
      , token = Some env:GITLAB_TOKEN as Text
      , tls
      }

let gitlabConsumer
    : Agents.GitlabConsumer
    = { name = "polar-gitlab-consumer"
      , image = img "polar-gitlab-consumer"
      , graph = Constants.graphConfig
      , tls
      }

let linker
    : Agents.ProvenanceLinker
    = { name = Constants.ProvenanceLinkerName
      , image = img "polar-linker-agent"
      , graph = Constants.graphConfig
      , tls
      }

let resolver
    : Agents.ProvenanceResolver
    = { name = Constants.ProvenanceResolverName
      , image = img "provenance-resolver-agent"
      , tls
      }

let gitObserver
    : Agents.GitObserver
    = { name = "git-repo-observer"
      , image = img "polar-git-observer"
      , config = ./conf/git.json as Text
      , tls
      }

let gitConsumer
    : Agents.GitConsumer
    = { name = "git-repo-consumer"
      , image = img "polar-git-processor"
      , graph = Constants.graphConfig
      , tls
      }

let gitScheduler
    : Agents.GitScheduler
    = { name = "git-scheduler"
      , image = img "polar-git-processor"
      , graph = Constants.graphConfig
      , tls
      }

let kubeObserver
    : Agents.KubeObserver
    = { name = "kube-observer"
      , serviceAccountName = "kube-observer-sa"
      , secretName = "kube-observer-sa-token"
      , image = img "polar-kube-observer"
      , tls
      }

let kubeConsumer
    : Agents.KubeConsumer
    = { name = "kube-consumer"
      , image = img "polar-kube-consumer"
      , graph = Constants.graphConfig
      , tls
      }

let buildOrchestrator: Agents.BuildOrchestrator = {
    name = "build-orchestrator"
    , image = img "polar-build-orchestrator"
    , tls
    , serviceAccountName = "build-processor-sa"
    , secretName = "build-processor-sa-token"
    , config = ./conf/cyclops.yaml as Text
    }

let buildProcessor
    : Agents.BuildProcessor
    = { name = "build-processor"
        , image = img "polar-build-processor"
        , graph = Constants.graphConfig
        , tls
        }

let clientTlsConfig
    : Agents.ClientTlsConfig
    = { broker_endpoint = Constants.cassiniDNSName
      , server_name = Constants.mtls.commonName
      , client_certificate_path = Constants.mtls.certPath
      , client_key_path = Constants.mtls.keyPath
      , client_ca_cert_path = Constants.mtls.caCertPath
      }

in  { imagePullSecrets
    , imagePullPolicy
    , cassini
    , cassiniTlsVolumeMount
    , gitlabObserver
    , gitlabConsumer
    , kubeObserver
    , kubeConsumer
    , linker
    , resolver
    , gitObserver
    , gitConsumer
    , gitScheduler
    , clientTlsConfig
    , neo4j
    , neo4jDNSName
    , neo4jBoltAddr
    , jaegerDNSName
    , buildProcessor
    , buildOrchestrator
    }
