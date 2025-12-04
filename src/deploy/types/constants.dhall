let kubernetes = ./kubernetes.dhall

let Agent = ./agents.dhall

-- TODO: Define a set of constants for environment variable strings
let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let SandboxRegistry =
      { url = "sandboxaksacr.azurecr.us"
      , imagePullSecrets =
        [ kubernetes.LocalObjectReference::{ name = Some "sandbox-registry" } ]
      }

let PolarNamespace = "polar"
let GraphNamespace = "polar-db"

let neo4jConfigmapName = "neo4j-config"
let graphSecretName = "polar-graph-pw"
let neo4jSecret =
      kubernetes.SecretKeySelector::{
      , key = "secret"
      , name = Some "neo4j-secret"
      }
let neo4jSecretEnv = kubernetes.EnvVar::{
          , name = "NEO4J_AUTH"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some neo4jSecret
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
      , certPath = "${tlsPath}/tls.crt"
      , keyPath = "${tlsPath}/tls.key"
      , proxyCertificate = "proxy-ca-cert"
      }

let cassiniPort = 8080

let cassiniService = { name = "cassini-ip-svc", type = "ClusterIP" }

let cassiniDNSName =
      "${cassiniService.name}.${PolarNamespace}.svc.cluster.local"

let cassiniAddr = "${cassiniDNSName}:${Natural/show cassiniPort}"

let commonClientTls
    : Agent.ClientTlsConfig
    = { broker_endpoint = cassiniAddr
      , server_name = cassiniDNSName
      , client_certificate_path = mtls.certPath
      , client_key_path = mtls.keyPath
      , client_ca_cert_path = mtls.caCertPath
      }

let polarAgentCertificateSpec =
      { commonName = mtls.commonName
      , dnsNames = [ cassiniDNSName ]
      , duration = "2160h"
      , issuerRef = { kind = "Issuer", name = mtls.leafIssuerName }
      , renewBefore = "360h"
      , secretName = "cassini-tls"
      }

let CassiniServerCertificateSecret = "cassini-tls"

let sandboxHostSuffix = "sandbox.labz.s-box.org"

let gitRepoSecret =
      kubernetes.SecretReference::{
      , name = Some "flux-repo-secret"
      , namespace = Some PolarNamespace
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



let graphPassword =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key = "secret"
      }

let graphConfig
    : Agent.GraphConfig
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

let commonClientEnv =
      [ kubernetes.EnvVar::{
        , name = "TLS_CA_CERT"
        , value = Some mtls.caCertPath
        }
      , kubernetes.EnvVar::{
        , name = "TLS_CLIENT_CERT"
        , value = Some mtls.certPath
        }
      , kubernetes.EnvVar::{
        , name = "TLS_CLIENT_KEY"
        , value = Some mtls.keyPath
        }
      , kubernetes.EnvVar::{ name = "BROKER_ADDR", value = Some cassiniAddr }
      , kubernetes.EnvVar::{
        , name = "CASSINI_SERVER_NAME"
        , value = Some cassiniDNSName
        }
      ]

let ClientTlsVolume =
      kubernetes.Volume::{
      , name = polarAgentCertificateSpec.secretName
      , secret = Some kubernetes.SecretVolumeSource::{
        , secretName = Some polarAgentCertificateSpec.secretName
        }
      }

let ClientTlsVolumeMount =
      kubernetes.VolumeMount::{
      , name = CassiniServerCertificateSecret
      , mountPath = tlsPath
      }

let neo4jServiceName = "polar-db-svc"

let neo4jDNSName = "${neo4jServiceName}.${GraphNamespace}.svc.cluster.local"

let ProvenanceDeploymentName = "polar-provenance"

let ProvenanceLinkerName = "provenance-linker"

let ProvenanceResolverName = "provenance-resolver"

let OciRegistrySecret =
      { name = "oci-registry-auth", value = env:OCI_REGISTRY_AUTH as Text }
let neo4jCredentialSecret = kubernetes.Secret::{
      apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
          name = Some "polar-graph-pw"
          , namespace = Some PolarNamespace
          }
       , immutable = Some True
      , stringData = Some [ { mapKey = neo4jSecret.key, mapValue = env:GRAPH_PASSWORD as Text } ]
      , type = Some "Opaque"
      }

let graphClientEnvVars = [      kubernetes.EnvVar::{
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

      in  { commitSha
    , SandboxRegistry
    , graphSecretName
    , tlsPath
    , CassiniServerCertificateSecret
    , RejectSidecarAnnotation
    , mtls
    , commonClientTls
    , ClientTlsVolume
    , PolarNamespace
    , GraphNamespace
    , sandboxHostSuffix
    , gitRepoSecret
    , deployRepository
    , neo4jConfigmapName
    , neo4jSecret
    , neo4jServiceName
    , graphPassword
    , graphConfig
    , graphClientEnvVars
    , DropAllCapSecurityContext
    , cassiniPort
    , cassiniService
    , cassiniAddr
    , cassiniDNSName
    , commonClientEnv
    , ProvenanceDeploymentName
    , ProvenanceLinkerName
    , ProvenanceResolverName
    , OciRegistrySecret
    }
