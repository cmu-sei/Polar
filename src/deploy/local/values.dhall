-- Some values specific to the environment
let Constants = ../types/constants.dhall

let kubernetes = ../types/kubernetes.dhall

let Cassini = ../types/cassini.dhall

let Agents = ../types/agents.dhall

let imagePullSecrets =
      [ kubernetes.LocalObjectReference::{
        , name = Some Constants.imagePullSecretName
        }
      ]

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let cassini
    : Cassini
    = { name = "cassini"
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/cassini:${commitSha}"
      , ports = { http = 3000, tcp = 8080 }
      , tls =
        { certificateRequestName = "cassini-certificate"
        , certificateSpec =
          { commonName = Constants.mtls.commonName
          , dnsNames = [ "cassini-ip-svc.polar.svc.cluster.local" ]
          , duration = "2160h"
          , issuerRef =
            { kind = "Issuer", name = Constants.mtls.leafIssuerName }
          , renewBefore = "360h"
          , secretName = Constants.CassiniServerCertificateSecret
          }
        }
      }

let cassiniTlsVolumeMount =
      kubernetes.VolumeMount::{
      , name = "client-tls"
      , mountPath = Constants.tlsPath
      }

let gitlabObserver
    : Agents.GitlabObserver
    = { name = "polar-gitlab-observer"
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-gitlab-observer:${commitSha}"
      , endpoint = "https://gitlab.sandbox.labz.s-box.org"
      , maxBackoffSecs = 300
      , baseIntervalSecs = 30
      , token = Some env:GITLAB_TOKEN as Text
      , tls = Constants.commonClientTls
      }

let gitlabConsumer
    : Agents.GitlabConsumer
    = { name = "polar-gitlab-consumer"
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-gitlab-consumer:${commitSha}"
      , graph = Constants.graphConfig
      , tls = Constants.commonClientTls
      }

let clientTlsConfig
    : Agents.ClientTlsConfig
    = { broker_endpoint = Constants.cassiniDNSName
      , server_name = Constants.mtls.commonName
      , client_certificate_path = Constants.mtls.certPath
      , client_key_path = Constants.mtls.keyPath
      , client_ca_cert_path = Constants.mtls.caCertPath
      }

let linker
    : Agents.ProvenanceLinker
    = { name = Constants.ProvenanceLinkerName
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-linker-agent:2de42e85"
      , graph = Constants.graphConfig
      , tls = Constants.commonClientTls
      }

let resolver
    : Agents.ProvenanceResolver
    = { name = Constants.ProvenanceLinkerName
      , image = "docker.io/library/provenance-resolver-agent:latest"
      , tls = Constants.commonClientTls
      }

let kubeAgent =
      { name = "kubernetes-agent"
      , observer =
        { name = "kube-observer"
        , serviceAccountName = "kube-observer-sa"
        , secretName = "kube-observer-sa-token"
        , image =
            "registry.sandbox.labz.s-box.org/sei/polar/polar-kube-observer:2de42e85"
        }
      , consumer =
        { name = "kube-consumer"
        , image =
            "registry.sandbox.labz.s-box.org/sei/polar/polar-kube-consumer:2de42e85"
        , graph =
          { graphDB = "neo4j"
          , graphUsername = "neo4j"
          , graphPassword = kubernetes.SecretKeySelector::{
            , name = Some "polar-graph-pw"
            , key = "secret"
            }
          }
        }
      , containerSecurityContext = kubernetes.SecurityContext::{
        , runAsGroup = Some 1000
        , runAsNonRoot = Some True
        , runAsUser = Some 1000
        , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
        }
      }

let gitObserverConfig = ./conf/git.json as Text

let gitObserver
    : Agents.GitObserver
    = { name = "git-repo-observer"
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-git-observer:2de42e85"
      , config = gitObserverConfig
      , tls = Constants.commonClientTls
      }

let gitConsumer
    : Agents.GitConsumer
    = { name = "git-repo-consumer"
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-git-processor:2de42e85"
      , tls = Constants.commonClientTls
      , graph = Constants.graphConfig
      }

let gitScheduler
    : Agents.GitScheduler
    = { name = "git-scheduler"
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-git-processor:2de42e85"
      , graph = Constants.graphConfig
      , tls = Constants.commonClientTls
      }

let kubeObserver
    : Agents.KubeObserver
    = { name = "kube-observer"
      , serviceAccountName = "kube-observer-sa"
      , secretName = "kube-observer-sa-token"
      , tls = Constants.commonClientTls
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-kube-observer:2de42e85"
      }

let kubeConsumer
    : Agents.KubeConsumer
    = { name = "kube-consumer"
      , tls = Constants.commonClientTls
      , image =
          "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-kube-consumer:2de42e85"
      , graph = Constants.graphConfig
      }

let neo4jPorts = { http = 7474, bolt = 7687 }

let neo4jHomePath = "/var/lib/neo4j"

let neo4j =
      { name = "polar-neo4j"
      , hostName = "neo4j"
      , namespace = Constants.GraphNamespace
      , image = "neo4j:5.26.2"
      , configVolume = "neo4j-config-copy"
      , certificatesVolume = "neo4j-certificates"
      , confDir = "/var/lib/neo4j/conf"
      , certDir = "/var/lib/neo4j/certificates"
      , ports = { http = 7474, bolt = 7687 }
      , config = { name = "neo4j-config", path = "/var/lib/neo4j/conf" }
      , volumes =
        { data =
          { name = "polar-db-data"
          , storageClassName = Some "standard"
          , storageSize = "10Gi"
          , mountPath = "/var/lib/neo4j/data"
          }
        , logs =
          { name = "polar-db-logs"
          , storageClassName = Some "standard"
          , storageSize = "10Gi"
          , mountPath = "/var/lib/neo4j/logs"
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
      "http://${jaeger.service.name}.${Constants.PolarNamespace}.svc.cluster.local:${Natural/show
                                                                                       jaeger.ports.http}/v1/traces"

in  { imagePullSecrets
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
    }
