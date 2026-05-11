let kubernetes = ./kubernetes.dhall

let Constants = ./constants.dhall

let Agents = ./agents.dhall

let GraphConfig = Agents.GraphConfig

let makeGraphEnv
    : Text →
      GraphConfig →
      kubernetes.SecretKeySelector.Type →
      Optional Text →
        List kubernetes.EnvVar.Type
    = λ(addr : Text) →
      λ(g : GraphConfig) →
      λ(passwordSecret : kubernetes.SecretKeySelector.Type) →
      λ(caCertPath : Optional Text) →
          [ kubernetes.EnvVar::{ name = "GRAPH_ENDPOINT", value = Some addr }
          , kubernetes.EnvVar::{ name = "GRAPH_DB", value = Some g.graphDB }
          , kubernetes.EnvVar::{
            , name = "GRAPH_USER"
            , value = Some g.graphUsername
            }
          , kubernetes.EnvVar::{
            , name = "GRAPH_PASSWORD"
            , valueFrom = Some kubernetes.EnvVarSource::{
              , secretKeyRef = Some passwordSecret
              }
            }
          ]
        # merge
            { Some =
                λ(path : Text) →
                  [ kubernetes.EnvVar::{
                    , name = "GRAPH_CA_CERT"
                    , value = Some path
                    }
                  ]
            , None = [] : List kubernetes.EnvVar.Type
            }
            caCertPath

let makeOpaqueSecret
    : Text → Text → Text → kubernetes.Secret.Type
    = λ(name : Text) →
      λ(key : Text) →
      λ(value : Text) →
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
    : Text → kubernetes.PodSpec.Type → kubernetes.Deployment.Type
    = λ(name : Text) →
      λ(podSpec : kubernetes.PodSpec.Type) →
        kubernetes.Deployment::{
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some Constants.PolarNamespace
          }
        , spec = Some kubernetes.DeploymentSpec::{
          , selector = kubernetes.LabelSelector::{
            , matchLabels = Some (toMap { name })
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
                    , secretName = Some certName
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
      λ(b64 : Text) →
      λ(name : Text) →
      λ(namespace : Text) →
        { apiVersion = "v1"
        , kind = "Secret"
        , metadata = { creationTimestamp = None Text, name, namespace }
        , data.`proxy.pem` = b64
        }

let DockerRegistrySecret =
      λ(secretName : Text) →
      λ(namespace : Text) →
      λ(dockerconfig_b64 : Text) →
        kubernetes.Secret::{
        , apiVersion = "v1"
        , kind = "Secret"
        , metadata = kubernetes.ObjectMeta::{
          , name = Some secretName
          , namespace = Some namespace
          }
        , type = Some "kubernetes.io/dockerconfigjson"
        , data = Some
          [ { mapKey = ".dockerconfigjson", mapValue = dockerconfig_b64 } ]
        }

let makeSaTokenVolume
    : Text → Text → Natural → kubernetes.Volume.Type
    = λ(saTokenVolumeName : Text) →
      λ(audience : Text) →
      λ(expirationSeconds : Natural) →
        kubernetes.Volume::{
        , name = saTokenVolumeName
        , projected = Some kubernetes.ProjectedVolumeSource::{
          , sources = Some
            [ kubernetes.VolumeProjection::{
              , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                , audience = Some audience
                , expirationSeconds = Some expirationSeconds
                , path = "token"
                }
              }
            ]
          }
        }

let makeCertVolume
    : Text → kubernetes.Volume.Type
    = λ(certVolumeName : Text) →
        kubernetes.Volume::{
        , name = certVolumeName
        , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        }

let makeSaTokenMount
    : Text -> Text -> kubernetes.VolumeMount.Type
    = \(saTokenVolumeName : Text)
    -> \(sa_token_path : Text)
    -> kubernetes.VolumeMount::{
        , name      = saTokenVolumeName
        , mountPath = "/home/polar/sa-token"
        , readOnly  = Some True
        }

let makeCertMount
    : Text → Text → kubernetes.VolumeMount.Type
    = λ(certVolumeName : Text) →
      λ(cert_dir : Text) →
        kubernetes.VolumeMount::{ name = certVolumeName, mountPath = cert_dir }

let makeNuInitScript
    : Text -> Text -> kubernetes.ConfigMap.Type
    = \(name : Text) -> \(script : Text) ->
        kubernetes.ConfigMap::{
        , apiVersion = "v1"
        , kind       = "ConfigMap"
        , metadata   = kubernetes.ObjectMeta::{
            , name      = Some name
            , namespace = Some Constants.PolarNamespace
            }
        , data = Some [ { mapKey = "init.nu", mapValue = script } ]
        }

let makeNuInitContainer
    : Text -> Agents.CertClientConfig -> Text -> Text -> Text -> kubernetes.Container.Type
    = \(nuInitImage : Text)
    -> \(cfg : Agents.CertClientConfig)
    -> \(saTokenVolumeName : Text)
    -> \(certVolumeName : Text)
    -> \(scriptVolumeName : Text)
    -> kubernetes.Container::{
        , name  = "polar-nu-init"
        , image = Some nuInitImage
        , env   = Some
            [ kubernetes.EnvVar::{ name = "POLAR_CERT_ISSUER_URL", value = Some cfg.cert_issuer_url }
            , kubernetes.EnvVar::{ name = "POLAR_SA_TOKEN_PATH",   value = Some cfg.sa_token_path }
            , kubernetes.EnvVar::{ name = "POLAR_CERT_DIR",        value = Some cfg.cert_dir }
            , kubernetes.EnvVar::{ name = "POLAR_CERT_TYPE",       value = Some cfg.cert_type }
            ]
        , volumeMounts = Some
            [ makeSaTokenMount saTokenVolumeName cfg.sa_token_path
            , makeCertMount    certVolumeName    cfg.cert_dir
            , kubernetes.VolumeMount::{
            , name      = scriptVolumeName
            , mountPath = "/scripts"
            , readOnly  = Some True
            }
            ]
        , securityContext = Some Constants.DropAllCapSecurityContext
        }

let makeCertVolumes
    : Text → Text → Text → Natural → List kubernetes.Volume.Type
    = λ(saTokenVolumeName : Text) →
      λ(certVolumeName : Text) →
      λ(audience : Text) →
      λ(expirationSeconds : Natural) →
        [ makeSaTokenVolume saTokenVolumeName audience expirationSeconds
        , makeCertVolume certVolumeName
        ]

let makeAgentCertMount
    : Text → Text → List kubernetes.VolumeMount.Type
    = λ(certVolumeName : Text) →
      λ(cert_dir : Text) →
        [ makeCertMount certVolumeName cert_dir ]

in  { makeGraphEnv
    , makeOpaqueSecret
    , makeDeployment
    , DockerRegistrySecret
    , mkProxySecret
    , ProxyVolume
    , ProxyMount
    , ProxyEnv
    , GraphProxyEnv
    , makeSaTokenVolume
    , makeCertVolume
    , makeSaTokenMount
    , makeCertMount
    , makeCertVolumes
    , makeAgentCertMount
    , makeNuInitScript
    , makeNuInitContainer
    }
