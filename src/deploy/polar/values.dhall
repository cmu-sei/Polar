let kubernetes = ../types/kubernetes.dhall

let namespace = "polar"

let sandboxHostSuffix = "sandbox.labz.s-box.org"

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let sandboxRegistry =
      { url = "sandboxaksacr.azurecr.us"
      , imagePullSecrets =
        [ kubernetes.LocalObjectReference::{ name = Some "sandbox-registry" } ]
      }

let gitRepoSecret =
      kubernetes.SecretReference::{
      , name = Some "flux-repo-secret"
      , namespace = Some namespace
      }

let deployRepository =
      { name = "polar-deploy-repo"
      , spec =
        { interval = "5m0s"
        , ref.branch = "sandbox"
        , url = "https://gitlab.sandbox.labz.s-box.org/sei/polar-deploy"
        , secretRef.name = gitRepoSecret.name
        }
      }

let RejectSidecarAnnotation =
      { mapKey = "sidecar.istio.io/inject", mapValue = "false" }

let tlsPath = "/etc/tls"

let mtls =
      { commonName = "polar"
      , caCertificateIssuerName = "ca-issuer"
      , caCertificateRequest = "ca-certificate"
      , caCertName = "ca-cert"
      , leafIssuerName = "polar-leaf-issuer"
      , caCertPath = "${tlsPath}/ca.crt"
      , serverCertPath = "${tlsPath}/tls.crt"
      , serverKeyPath = "${tlsPath}/tls.key"
      , proxyCertificate = "proxy-ca-cert"
      }

let cassiniPort = 8080

let cassiniService = { name = "cassini-ip-svc", type = "ClusterIP" }

let cassiniDNSName = "${cassiniService.name}.${namespace}.svc.cluster.local"

let cassiniAddr = "${cassiniDNSName}:${Natural/show cassiniPort}"

let cassiniServerCertificateSecret = "cassini-tls"

let cassini =
      { name = "cassini"
      , namespace
      , image = "${sandboxRegistry.url}/polar/cassini:${commitSha}"
      , imagePullSecrets = sandboxRegistry.imagePullSecrets
      , containerSecurityContext = kubernetes.SecurityContext::{
        , runAsGroup = Some 1000
        , runAsNonRoot = Some True
        , runAsUser = Some 1000
        , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
        }
      , podAnnotations = [ RejectSidecarAnnotation ]
      , port = cassiniPort
      , service = cassiniService
      , tls =
        { certificateRequestName = "cassini-certificate"
        , certificateSpec =
          { commonName = mtls.commonName
          , dnsNames = [ cassiniDNSName ]
          , duration = "2160h"
          , issuerRef = { kind = "Issuer", name = mtls.leafIssuerName }
          , renewBefore = "360h"
          , secretName = cassiniServerCertificateSecret
          }
        }
      , environment =
        [ kubernetes.EnvVar::{
          , name = "TLS_CA_CERT"
          , value = Some mtls.caCertPath
          }
        , kubernetes.EnvVar::{
          , name = "TLS_SERVER_CERT_CHAIN"
          , value = Some mtls.serverCertPath
          }
        , kubernetes.EnvVar::{
          , name = "TLS_SERVER_KEY"
          , value = Some mtls.serverKeyPath
          }
        , kubernetes.EnvVar::{
          , name = "CASSINI_BIND_ADDR"
          , value = Some "0.0.0.0:${Natural/show cassiniPort}"
          }
        ]
      , volumes =
        [ kubernetes.Volume::{
          , name = cassiniServerCertificateSecret
          , secret = Some kubernetes.SecretVolumeSource::{
            , secretName = Some cassiniServerCertificateSecret
            }
          }
        ]
      , volumeMounts =
        [ kubernetes.VolumeMount::{
          , name = cassiniServerCertificateSecret
          , mountPath = tlsPath
          , readOnly = Some True
          }
        ]
      }

let graphSecret =
      kubernetes.SecretKeySelector::{
      , key = "secret"
      , name = Some "neo4j-secret"
      }

let gitlabSecret =
      kubernetes.SecretKeySelector::{
      , key = "token"
      , name = Some "gitlab-secret"
      }

let polarClientCertificateSecret = "client-tls"

let polarAgentCertificateSpec =
      { commonName = mtls.commonName
      , dnsNames = [ cassiniDNSName ]
      , duration = "2160h"
      , issuerRef = { kind = "Issuer", name = mtls.leafIssuerName }
      , renewBefore = "360h"
      , secretName = polarClientCertificateSecret
      }

let gitlab =
      { name = "gitlab-agent"
      , serviceAccountName = "gitlab-agent-sa"
      , podAnnotations = [ RejectSidecarAnnotation ]
      , imagePullSecrets = sandboxRegistry.imagePullSecrets
      , containerSecurityContext = kubernetes.SecurityContext::{
        , runAsGroup = Some 1000
        , runAsNonRoot = Some True
        , runAsUser = Some 1000
        , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
        }
      , tls =
        { certificateRequestName = "gitlab-agent-certificate"
        , certificateSpec = polarAgentCertificateSpec
        }
      , observer =
        { name = "polar-gitlab-observer"
        , image =
            "${sandboxRegistry.url}/polar/polar-gitlab-observer:${commitSha}"
        , gitlabEndpoint = "https://gitlab.sandbox.labz.s-box.org/api/graphql"
        , gitlabSecret
        }
      , consumer =
        { name = "polar-gitlab-consumer"
        , image =
            "${sandboxRegistry.url}/polar/polar-gitlab-consumer:${commitSha}"
        , graph =
          { graphDB = "neo4j"
          , graphUsername = "neo4j"
          , graphPassword = kubernetes.SecretKeySelector::{
            , name = Some "polar-graph-pw"
            , key = "secret"
            }
          }
        }
      }

let kubeAgent =
      { name = "kubernetes-agent"
      , tls =
        { certificateRequestName = "kube-agent-certificate"
        , certificateSpec = polarAgentCertificateSpec
        }
      , podAnnotations = [ RejectSidecarAnnotation ]
      , observer =
        { name = "kube-observer"
        , serviceAccountName = "kube-observer-sa"
        , secretName = "kube-observer-sa-token"
        , image =
            "${sandboxRegistry.url}/polar/polar-kube-observer:${commitSha}"
        }
      , consumer =
        { name = "kube-consumer"
        , image =
            "${sandboxRegistry.url}/polar/polar-kube-consumer:${commitSha}"
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

let neo4jPorts = { https = 7473, bolt = 7687 }

let neo4jHomePath = "/var/lib/neo4j"

let neo4j =
      { name = "polar-neo4j"
      , hostName = "graph-db.sandbox.labz.s-box.org"
      , namespace = "polar-db"
      , image = "${sandboxRegistry.url}/ironbank/opensource/neo4j/neo4j:5.26.2"
      , imagePullSecrets = sandboxRegistry.imagePullSecrets
      , config = { name = "neo4j-config", path = "/var/lib/neo4j/conf" }
      , env =
        [ kubernetes.EnvVar::{
          , name = "NEO4J_AUTH"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some graphSecret
            }
          }
        ]
      , containerPorts =
        [ kubernetes.ContainerPort::{ containerPort = neo4jPorts.https }
        , kubernetes.ContainerPort::{ containerPort = neo4jPorts.bolt }
        ]
      , service.name = "polar-db-svc"
      , logging = { serverLogsXml = "", userLogsXml = "" }
      , resources = { cpu = "2000m", memory = "2Gi" }
      , podSecurityContext = kubernetes.PodSecurityContext::{
        , fsGroup = Some 7474
        , fsGroupChangePolicy = Some "OnRootMismatch"
        }
      , containerSecurityContext = kubernetes.SecurityContext::{
        , runAsGroup = Some 7474
        , runAsNonRoot = Some True
        , runAsUser = Some 7474
        , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
        }
      , tls =
        { caIssuerName = "selfsigned-root"
        , leafIssuer = "db-ca-issuer"
        , caSecretName = "root-ca-secret"
        , leafSecretName = "neo4j-keypair"
        }
      , volumes =
        { data =
          { name = "polar-db-data"
          , storageClassName = Some "managed-csi"
          , storageSize = "50Gi"
          , mountPath = "/var/lib/neo4j/data"
          }
        , logs =
          { name = "polar-db-logs"
          , storageClassName = Some "managed-csi"
          , storageSize = "50Gi"
          , mountPath = "/var/lib/neo4j/logs"
          }
        }
      }

let neo4jDNSName = "${neo4j.service.name}.${neo4j.namespace}.svc.cluster.local"

let neo4jBoltAddr = "neo4j://${neo4jDNSName}:7687"

let neo4jUiAddr = "${neo4jDNSName}:${Natural/show neo4jPorts.https}"

in  { namespace
    , deployRepository
    , sandboxHostSuffix
    , sandboxRegistry
    , mtls
    , tlsPath
    , cassini
    , cassiniDNSName
    , cassiniAddr
    , neo4jPorts
    , neo4j
    , graphSecret
    , neo4jDNSName
    , neo4jUiAddr
    , neo4jBoltAddr
    , gitlab
    , gitlabSecret
    , kubeAgent
    }
