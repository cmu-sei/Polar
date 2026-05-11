{-
  local/cassini.dhall — Cassini broker deployment for local cluster development.

  Uses the polar-nu-init image to run the cert bootstrap script at pod
  startup. The nushell script is mounted from a ConfigMap at /scripts/init.nu.
  The nu-init entrypoint checks for the script, executes it, and exits.
  Cassini starts only after the init container exits zero.
-}

let kubernetes = ../../types/kubernetes.dhall
let Constants  = ../../types/constants.dhall
let functions  = ../../types/functions.dhall
let values     = ../values.dhall

let cassini = values.cassini

let nuInitImage       = "polar-nu-init:${Constants.commitSha}"
let scriptVolumeName  = Constants.initScriptVolumeName
let scriptConfigName  = Constants.initScriptConfigMapName
-- =============================================================================
-- Service
-- =============================================================================

let cassiniService =
      kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some Constants.cassiniService.name
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.ServiceSpec::{
        , selector = Some (toMap { name = cassini.name })
        , type     = Some Constants.cassiniService.type
        , ports    = Some
          [ kubernetes.ServicePort::{
            , name       = Some "cassini-tcp"
            , port       = cassini.ports.tcp
            , targetPort = Some (kubernetes.NatOrString.Nat cassini.ports.tcp)
            }
          , kubernetes.ServicePort::{
            , name       = Some "cassini-http"
            , port       = cassini.ports.http
            , targetPort = Some (kubernetes.NatOrString.Nat cassini.ports.http)
            }
          ]
        }
      }

-- =============================================================================
-- Service account
-- =============================================================================

let cassiniServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind       = "ServiceAccount"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some cassini.serviceAccountName
        , namespace = Some Constants.PolarNamespace
        }
      , automountServiceAccountToken = Some False
      }

-- =============================================================================
-- ConfigMap carrying the nushell init script
-- =============================================================================

let certInitScriptConfigMap =
      functions.makeNuInitScript
        scriptConfigName
        Constants.polarInitScript

-- =============================================================================
-- Volumes
-- =============================================================================

let certVolumes =
      functions.makeCertVolumes
        Constants.saTokenVolumeName
        Constants.certVolumeName
        cassini.certClient.audience
        Constants.certTokenExpiry

let scriptVolume =
      kubernetes.Volume::{
      , name      = scriptVolumeName
      , configMap = Some kubernetes.ConfigMapVolumeSource::{
        , name        = Some scriptConfigName
        }
      }

let allVolumes = certVolumes # [ scriptVolume ]

-- =============================================================================
-- Init container
-- =============================================================================

let certInitContainer =
(functions.makeNuInitContainer
        nuInitImage
        (cassini.certClient // { cert_type = "server" })
        Constants.saTokenVolumeName
        Constants.certVolumeName
        scriptVolumeName) // { imagePullPolicy = Some values.imagePullPolicy }
-- =============================================================================
-- Cassini container
-- =============================================================================

let environment =
      [ kubernetes.EnvVar::{ name = "TLS_CA_CERT",           value = Some cassini.tls.ca_cert_path }
      , kubernetes.EnvVar::{ name = "TLS_SERVER_CERT_CHAIN",  value = Some cassini.tls.server_cert_path }
      , kubernetes.EnvVar::{ name = "TLS_SERVER_KEY",         value = Some cassini.tls.server_key_path }
      , kubernetes.EnvVar::{
        , name  = "CASSINI_BIND_ADDR"
        , value = Some "0.0.0.0:${Natural/show cassini.ports.tcp}"
        }
      , kubernetes.EnvVar::{
        , name  = "JAEGER_OTLP_ENDPOINT"
        , value = Some values.jaegerDNSName
        }
      ]

let cassiniContainer =
      kubernetes.Container::{
      , name            = cassini.name
      , image           = Some cassini.image
      , imagePullPolicy = Some values.imagePullPolicy
      , securityContext = Some Constants.DropAllCapSecurityContext
      , env             = Some environment
      , ports           = Some
        [ kubernetes.ContainerPort::{ containerPort = cassini.ports.tcp }
        , kubernetes.ContainerPort::{ containerPort = cassini.ports.http }
        ]
      , volumeMounts = Some
          ( functions.makeAgentCertMount
              Constants.certVolumeName
              cassini.certClient.cert_dir
          )
      }

-- =============================================================================
-- Deployment
-- =============================================================================

let deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some cassini.name
        , namespace = Some Constants.PolarNamespace
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , replicas = Some 1
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some (toMap { name = cassini.name })
          }
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name   = Some cassini.name
            , labels = Some [ { mapKey = "name", mapValue = cassini.name } ]
            }
          , spec = Some kubernetes.PodSpec::{
            , serviceAccountName = Some cassini.serviceAccountName
            , imagePullSecrets   = Some values.imagePullSecrets
            , initContainers     = Some [ certInitContainer ]
            , containers         = [ cassiniContainer ]
            , volumes            = Some allVolumes
            }
          }
        }
      }

in  [ kubernetes.Resource.ConfigMap     certInitScriptConfigMap
    , kubernetes.Resource.ServiceAccount cassiniServiceAccount
    , kubernetes.Resource.Service        cassiniService
    , kubernetes.Resource.Deployment     deployment
    ]
