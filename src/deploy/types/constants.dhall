let kubernetes = ./kubernetes.dhall

let Agents = ./agents.dhall

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let imagePullSecretName = "registry-secret"

let PolarNamespace = env:NAMESPACE as Text ? "polar"

let GraphNamespace = env:GRAPH_NAMESPACE as Text ? "polar-db"

let polarInitScript = ../../../scripts/polar-init.nu as Text

let polarHomeDir = "/home/polar"

let neo4jConfigmapName = "neo4j-config"

let initScriptVolumeName  = "cert-init-script"

let neo4jSecret =
      kubernetes.SecretKeySelector::{
      , key = "secret"
      , name = Some "neo4j-secret"
      }

let graphSecretKeySelector =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key = "secret"
      }

let neo4jSecretEnv =
      kubernetes.EnvVar::{
      , name = "NEO4J_AUTH"
      , valueFrom = Some kubernetes.EnvVarSource::{
        , secretKeyRef = Some neo4jSecret
        }
      }

let RejectSidecarAnnotation =
      { mapKey = "sidecar.istio.io/inject", mapValue = "false" }

let certDir = "/home/polar/certs"

let certPaths =
      { ca = "${certDir}/ca.pem"
      , cert = "${certDir}/cert.pem"
      , key = "${certDir}/key.pem"
      }

let cassiniPort = 8080

let cassiniService = { name = "cassini-ip-svc", type = "ClusterIP" }

let cassiniDNSName =
      "${cassiniService.name}.${PolarNamespace}.svc.cluster.local"

let cassiniAddr = "${cassiniDNSName}:${Natural/show cassiniPort}"

let cassiniServerName = "cassini.polar.serviceaccount.cluster.local"

let commonClientTls
    : Agents.ClientTlsConfig
    = { broker_endpoint = cassiniAddr
      , server_name = cassiniServerName
      , client_certificate_path = certPaths.cert
      , client_key_path = certPaths.key
      , client_ca_cert_path = certPaths.ca
      }

let commonClientEnv =
      [ kubernetes.EnvVar::{ name = "TLS_CA_CERT", value = Some certPaths.ca }
      , kubernetes.EnvVar::{
        , name = "TLS_CLIENT_CERT"
        , value = Some certPaths.cert
        }
      , kubernetes.EnvVar::{
        , name = "TLS_CLIENT_KEY"
        , value = Some certPaths.key
        }
      , kubernetes.EnvVar::{ name = "BROKER_ADDR", value = Some cassiniAddr }
      , kubernetes.EnvVar::{
        , name = "CASSINI_SERVER_NAME"
        , value = Some cassiniServerName
        }
      ]

let defaultCertClientConfig
    : Agents.CertClientConfig
    = { sa_token_path   = "/home/polar/sa-token/token"
      , cert_issuer_url =
          "http://cert-issuer.${PolarNamespace}.svc.cluster.local:8443"
      , audience = "polar-cert-issuer.local"
      , cert_dir = certDir
      , cert_type = "client"
      }

let saTokenVolumeName = "sa-token"

let certVolumeName = "polar-certs"

let certTokenExpiry = 3600

let graphSecretName = "polar-graph-pw"

let graphPassword =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key = "secret"
      }

let graphConfig
    : Agents.GraphConfig
    = { graphDB = "neo4j"
      , graphUsername = "neo4j"
      , graphPassword = neo4jSecret
      }

let DropAllCapSecurityContext =
      kubernetes.SecurityContext::{
      , runAsGroup = Some 1000
      , runAsNonRoot = Some True
      , runAsUser = Some 1000
      , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
      }

let graphClientEnvVars =
      [ kubernetes.EnvVar::{
        , name = "GRAPH_DB"
        , value = Some graphConfig.graphDB
        }
      , kubernetes.EnvVar::{
        , name = "GRAPH_USER"
        , value = Some graphConfig.graphUsername
        }
      , kubernetes.EnvVar::{
        , name = "GRAPH_PASSWORD"
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{
            , name = Some "polar-graph-pw"
            , key = neo4jSecret.key
            }
          }
        }
      ]

let gitRepoSecret =
      kubernetes.SecretReference::{
      , name = Some "flux-repo-secret"
      , namespace = Some PolarNamespace
      }

let gitlabSecretKeySelector =
      kubernetes.SecretKeySelector::{
      , key = "token"
      , name = Some "gitlab-secret"
      }

let neo4jServiceName = "polar-db-svc"

let neo4jDNSName = "${neo4jServiceName}.${GraphNamespace}.svc.cluster.local"

let RegistryResolverName = "oci-registry-resolver"

let ArtifactLinkerName = "artifact-linker"

let ProvenanceLinkerName = "provenance-linker"

let ProvenanceResolverName = "provenance-resolver"

let OciRegistrySecret =
      { name = "oci-registry-auth"
      , value = env:DOCKER_AUTH_JSON as Text ? "someJson"
      }

let neo4jCredentialSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "polar-graph-pw"
        , namespace = Some PolarNamespace
        }
      , immutable = Some True
      , stringData = Some
        [ { mapKey = neo4jSecret.key
          , mapValue = env:GRAPH_PASSWORD as Text ? "somepassword"
          }
        ]
      , type = Some "Opaque"
      }

let gitObserverSecretName = "git-observer-secret"

let saTokenDir  = "/home/polar/sa-token"
let saTokenFile = "${saTokenDir}/token"
let initScriptConfigMapName = "cert-init-script"

in  { graphSecretKeySelector
    , saTokenDir
    , saTokenFile
    , polarInitScript
    , defaultCertClientConfig
    , imagePullSecretName
    , commitSha
    , gitlabSecretKeySelector
    , graphSecretName
    , certDir
    , certPaths
    , saTokenVolumeName
    , certVolumeName
    , certTokenExpiry
    , RejectSidecarAnnotation
    , commonClientTls
    , PolarNamespace
    , GraphNamespace
    , gitRepoSecret
    , neo4jConfigmapName
    , neo4jSecret
    , neo4jServiceName
    , neo4jDNSName
    , graphPassword
    , graphConfig
    , graphClientEnvVars
    , DropAllCapSecurityContext
    , cassiniPort
    , cassiniService
    , cassiniAddr
    , cassiniDNSName
    , cassiniServerName
    , commonClientEnv
    , RegistryResolverName
    , ArtifactLinkerName
    , ProvenanceLinkerName
    , ProvenanceResolverName
    , OciRegistrySecret
    , gitObserverSecretName
    , neo4jCredentialSecret
    , initScriptVolumeName
    , initScriptConfigMapName
    }
