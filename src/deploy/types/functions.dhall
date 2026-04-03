let kubernetes = ./kubernetes.dhall
let Constants = ./constants.dhall
let Agents = ./agents.dhall
let GraphConfig = Agents.GraphConfig
--let Certificate = ./types/certificate.dhall

--let GetCertificate = \(name : Text) ->
--    \(namespace: Text) ->
--    \(commonName : Text) ->
--    \(secretName : Text) ->
--    \(dnsNames : List Text) ->
--      Certificate::{
--        apiVersion = "cert-manager.io/v1"
--      , kind = "Certificate"
--      , metadata = { name = name, namespace = namespace }
--      , spec =
--          { commonName = commonName
--          , dnsNames = dnsNames
--          , duration = "2160h"
--          , issuerRef = { kind = "Issuer", name = "${commonName}-leaf-issuer" }
--          , renewBefore = "360h"
--          , secretName = secretName
--          }
--      }
-- helper to create a registry secret provided a name, namespace, and base64 encoded config.json

let makeGraphEnv
    : Text -> GraphConfig -> kubernetes.SecretKeySelector.Type -> List kubernetes.EnvVar.Type
    = \(addr : Text) -> \(g : GraphConfig) -> \(passwordSecret : kubernetes.SecretKeySelector.Type) ->
      [ kubernetes.EnvVar::{ name = "GRAPH_ENDPOINT", value = Some addr }
      , kubernetes.EnvVar::{ name = "GRAPH_DB", value = Some g.graphDB }
      , kubernetes.EnvVar::{ name = "GRAPH_USER", value = Some g.graphUsername }
      , kubernetes.EnvVar::{
        , name = "GRAPH_PASSWORD"
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some passwordSecret
          }
        }
      ]

let makeOpaqueSecret
    : Text -> Text -> Text -> kubernetes.Secret.Type
    = \(name : Text) -> \(key : Text) -> \(value : Text) ->
        kubernetes.Secret::{
        , apiVersion = "v1"
        , kind = "Secret"
        , metadata = kubernetes.ObjectMeta::{
            , name = Some name
            , namespace = Some Constants.PolarNamespace
            }
        , stringData = Some [ { mapKey = key, mapValue = value } ]
        , type = Some "Opaque"
        }

let makeDeployment
    : forall (name : Text) -> kubernetes.PodSpec.Type -> kubernetes.Deployment.Type
    = \(name : Text) -> \(podSpec : kubernetes.PodSpec.Type) ->
        kubernetes.Deployment::{
          , metadata = kubernetes.ObjectMeta::{
            , name = Some name
            , namespace = Some Constants.PolarNamespace
            }
          , spec = Some kubernetes.DeploymentSpec::{
            , selector = kubernetes.LabelSelector::{
              , matchLabels = Some (toMap { name = name })
              }
            , replicas = Some 1
            , template = kubernetes.PodTemplateSpec::{
              , metadata = Some kubernetes.ObjectMeta::{
                , name = Some name
                , labels = Some [ { mapKey = "name", mapValue = name } ]
                }
              , spec = Some podSpec
              }
            }
          }

let ProxyVolume =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(certName : Text) →
                [ kubernetes.Volume::{
                    , name = certName
                    , secret = Some kubernetes.SecretVolumeSource::{
                        secretName = Some certName
                    }
                  }
                ]
          , None = [] : List kubernetes.Volume.Type
          }
          cert

let ProxyMount =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(cert : Text) →
                [ kubernetes.VolumeMount::{
                    , name = cert
                    , mountPath = "/etc/tls/proxy"
                    , readOnly = Some True
                  }
                ]
          , None = [] : List kubernetes.VolumeMount.Type
          }
          cert

-- Adds an environment variable to point to a path where a proxy CA certificate will be mounted
let ProxyEnv =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(_ : Text) →
                [ kubernetes.EnvVar::{
                    , name = "PROXY_CA_CERT"
                    , value = Some "/etc/tls/proxy/proxy.crt"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert
-- Sets the GRAPH_CA_CERT env var to point to a proxy cert that might sit in front of neo4j

let GraphProxyEnv =
    λ(cert : Optional Text) →
        merge
        { Some =
            λ(_ : Text) →
                [ kubernetes.EnvVar::{
                    , name = "GRAPH_CA_CERT"
                    , value = Some "/etc/tls/proxy/proxy.crt"
                }
                ]
        , None = [] : List kubernetes.EnvVar.Type
        }
        cert

let mkProxySecret =
      \(b64 : Text) ->
      \(name : Text) ->
      \(namespace : Text) ->
        { apiVersion = "v1"
        , kind = "Secret"
        , metadata =
            { creationTimestamp = None Text
            , name = name
            , namespace = namespace
            }
        , data =
            { `proxy.pem` = b64 }
        }

let DockerRegistrySecret =
          \(secretName : Text) ->
          \(namespace : Text) ->
          \(dockerconfig_b64 : Text) ->
            kubernetes.Secret::{
            , apiVersion = "v1"
            , kind = "Secret"
            , metadata = kubernetes.ObjectMeta::{
                , name = Some secretName
                , namespace = Some namespace
              }
            , type = Some "kubernetes.io/dockerconfigjson"
            , data = Some
                [ { mapKey = ".dockerconfigjson"
                  , mapValue = dockerconfig_b64
                  }
                ]
            }

in { makeGraphEnv, makeOpaqueSecret, makeDeployment, DockerRegistrySecret, mkProxySecret, ProxyVolume, ProxyMount, ProxyEnv, GraphProxyEnv }
