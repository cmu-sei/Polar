let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let Agents = ./agents.dhall

let GraphConfig = Agents.GraphConfig

let PolarNamespace = C.polarNamespace

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
          , namespace = Some PolarNamespace
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
          , namespace = Some PolarNamespace
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
    : Text -> kubernetes.VolumeMount.Type
    = \(saTokenVolumeName : Text)
    -> kubernetes.VolumeMount::{
        , name      = saTokenVolumeName
        , mountPath = C.saTokenDir
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
            , namespace = Some PolarNamespace
            }
        , data = Some [ { mapKey = "init.nu", mapValue = script } ]
        }

let makeNuInitContainer
    : Text -> Agents.CertClientConfig.Type -> Text -> Text -> Text -> kubernetes.SecurityContext.Type -> kubernetes.Container.Type
    = \(nuInitImage : Text)
    -> \(cfg : Agents.CertClientConfig.Type)
    -> \(saTokenVolumeName : Text)
    -> \(certVolumeName : Text)
    -> \(scriptVolumeName : Text)
    -> \(securityContext : kubernetes.SecurityContext.Type)
    -> kubernetes.Container::{
        , name  = "polar-nu-init"
        , image = Some nuInitImage
        , env   = Some
            [ kubernetes.EnvVar::{ name = "POLAR_CERT_ISSUER_URL", value = Some cfg.cert_issuer_url }
            , kubernetes.EnvVar::{ name = "POLAR_SA_TOKEN_PATH",   value = Some cfg.sa_token_path }
            , kubernetes.EnvVar::{ name = "POLAR_CERT_DIR",        value = Some cfg.cert_dir }
            , kubernetes.EnvVar::{ name = "POLAR_CERT_TYPE",       value = Some cfg.cert_type }
            , kubernetes.EnvVar::{ name = "POLAR_KEY_ALGORITHM", value = Some cfg.key_algorithm }
            , kubernetes.EnvVar::{
              , name  = "POLAR_EXTRA_SANS"
              , value =  cfg.extra_sans
              }
            ]
        , volumeMounts = Some
            [ makeSaTokenMount saTokenVolumeName
            , makeCertMount    certVolumeName    cfg.cert_dir
            , kubernetes.VolumeMount::{
            , name      = scriptVolumeName
            , mountPath = "/scripts"
            , readOnly  = Some True
            }
            ]
        , securityContext = Some securityContext
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

{-
NOTE:
I'm left wondering what pieces of the functions.dhall should be exported and which should remain private.

I think the right question for each function is: can a consumer meaningfully call this without knowing Polar's internal conventions?
If the answer is no — meaning that calling it correctly requires knowing C.saTokenDir,
or the projected volume path convention,
or the cert directory layout —
then it's an internal helper, not a public API.

I think we can break down most of the lib into three categories:

Clearly public — these are genuinely useful to any consumer writing a Polar agent deployment and require no internal knowledge to call correctly:

makeDeployment,
makeOpaqueSecret,
makeGraphEnv,
ProxyEnv,
ProxyVolume,
ProxyMount.

These take explicit parameters and produce correct output without the caller needing to know anything about Polar's internal wiring.

Clearly internal — makeSaTokenMount, makeCertMount, makeSaTokenVolume.
These are only correct when called with C.saTokenDir, C.certDir, and the specific volume name conventions.
A consumer who calls makeSaTokenMount with the wrong directory gets a broken pod.
The function's correctness is inseparable from constants the consumer shouldn't need to know.

Genuinely ambiguous — makeNuInitContainer, makeCertVolumes, makeAgentCertMount.
These are the interesting ones. They encode the full cert bootstrap pattern
— which is exactly what a consumer needs if they're deploying a Polar agent.

But they also close over C.saTokenDir now,
and their correct use requires understanding the init container / projected volume / cert mount relationship.
You could argue they're the highest-value exports precisely because they encode that complexity correctly so consumers don't have to.

-}
in  { makeGraphEnv
    , makeOpaqueSecret
    , makeDeployment
    , makeSaTokenVolume
    , DockerRegistrySecret
    , mkProxySecret
    , ProxyVolume
    , ProxyMount
    , ProxyEnv
    , GraphProxyEnv
    , makeCertVolume
    , makeCertVolumes
    , makeAgentCertMount
    , makeNuInitScript
    , makeNuInitContainer
    }
