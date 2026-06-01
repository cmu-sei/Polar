{-
  local/neo4j.dhall — Neo4j deployment for local cluster development.

  Two init containers run in sequence:
    1. polar-nu-init with polar-init.nu  — cert bootstrap (server cert)
    2. alpine with neo4j-init.sh         — conf copy, cert layout, chown

  Each container mounts its own script ConfigMap at /scripts/init.nu so
  the polar-nu-init entrypoint finds exactly the right script in both cases.
  The two ConfigMaps are distinct — certInitScriptCmName and neo4jInitScriptCmName.
-}

let kubernetes = ../../types/kubernetes.dhall
let C          = ../../types/lib-constants.dhall
let Polar      = ../../types/package.dhall
let Agent      = Polar.agents
let functions  = Polar.functions
let values     = ../values.dhall

let polarInitScript = ../../../../scripts/polar-init.nu as Text
let neo4jInitScript = ../../../../scripts/setup-neo4j.sh as Text

let configContent =
      if values.neo4j.enableTls
      then ../conf/neo4j-ssl.conf as Text
      else ../conf/neo4j-no-ssl.conf as Text

-- -------------------------------------------------------------------------
-- Deployment-local constants
-- -------------------------------------------------------------------------

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let nuInitImage = "polar-nu-init:${commitSha}"

let certTokenExpiry = 3600

-- Neo4j runs under UID/GID 7474 — different from the standard polar 1000.
let neo4jSecurityContext =
      kubernetes.SecurityContext::{
      , runAsGroup   = Some 7474
      , runAsNonRoot = Some True
      , runAsUser    = Some 7474
      , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
      }

let dropAllCapSecurityContext =
      kubernetes.SecurityContext::{
      , runAsGroup   = Some 1000
      , runAsNonRoot = Some True
      , runAsUser    = Some 1000
      , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
      }

-- Neo4j's cert directory is separate from the standard polar certDir —
-- it follows Neo4j's expected layout under its data directory.
let neo4jCertDir = "/var/lib/neo4j/certificates"

-- Volume names are local pod wiring — only need to be consistent within
-- this file's pod spec.
let neo4jSaTokenVolume    = "neo4j-sa-token"
let neo4jCertVolume       = "neo4j-polar-certs"
let certInitScriptVolume  = "neo4j-cert-init-script"
let certInitScriptCmName  = "neo4j-cert-init-script"
let neo4jInitScriptVolume = "neo4j-setup-script"
let neo4jInitScriptCmName = "neo4j-setup-script"
let neo4jConfigmapName    = "neo4j-config"

-- The SecretKeySelector for the neo4j auth secret. The secret name and key
-- are fixed by the Secret manifest emitted in this file.
let neo4jSecretSelector =
      kubernetes.SecretKeySelector::{
      , name = Some "neo4j-secret"
      , key  = "secret"
      }

-- CertClientConfig for Neo4j's cert bootstrap. Overrides the standard
-- defaults: different cert directory, server cert type, and extra SANs
-- for the DNS names Neo4j will be reachable at.
let neo4jCertClientConfig
    : Agent.CertClientConfig.Type
    =     Agent.CertClientConfig.default
      //  { cert_dir   = neo4jCertDir
          , cert_type  = "server"
          , extra_sans = Some "neo4j.polar.svc.cluster.local,polar-db-svc.polar.svc.cluster.local"
          }

-- -------------------------------------------------------------------------
-- Service account
-- -------------------------------------------------------------------------

let neo4jServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind       = "ServiceAccount"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some "neo4j-sa"
        , namespace = Some C.polarNamespace
        }
      , automountServiceAccountToken = Some False
      }

-- -------------------------------------------------------------------------
-- ConfigMaps
-- -------------------------------------------------------------------------

let neo4jConfigMap =
      kubernetes.ConfigMap::{
      , apiVersion = "v1"
      , kind       = "ConfigMap"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some neo4jConfigmapName
        , namespace = Some C.polarNamespace
        }
      , data = Some [ { mapKey = "neo4j.conf", mapValue = configContent } ]
      }

-- Script for init container 1: cert bootstrap via polar-nu-init
let certInitScriptConfigMap =
      kubernetes.ConfigMap::{
      , apiVersion = "v1"
      , kind       = "ConfigMap"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some certInitScriptCmName
        , namespace = Some C.polarNamespace
        }
      , data = Some [ { mapKey = "init.nu", mapValue = polarInitScript } ]
      }

-- Script for init container 2: neo4j setup (conf copy, cert layout, chown)
let neo4jInitScriptConfigMap =
      kubernetes.ConfigMap::{
      , apiVersion = "v1"
      , kind       = "ConfigMap"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some neo4jInitScriptCmName
        , namespace = Some C.polarNamespace
        }
      , data = Some [ { mapKey = "init.nu", mapValue = neo4jInitScript } ]
      }

-- -------------------------------------------------------------------------
-- Secret
-- -------------------------------------------------------------------------

let neo4jSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind       = "Secret"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some "neo4j-secret"
        , namespace = Some C.polarNamespace
        }
      , stringData = Some
          [ { mapKey = neo4jSecretSelector.key, mapValue = env:NEO4J_AUTH as Text } ]
      , type = Some "Opaque"
      }

-- -------------------------------------------------------------------------
-- PVCs
-- -------------------------------------------------------------------------

let dataVolumeClaim =
      kubernetes.PersistentVolumeClaim::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some values.neo4j.volumes.data.name
        , namespace = Some C.polarNamespace
        }
      , spec = Some kubernetes.PersistentVolumeClaimSpec::{
        , accessModes      = Some [ "ReadWriteOnce" ]
        , storageClassName = values.neo4j.volumes.data.storageClassName
        , resources        = Some kubernetes.VolumeResourceRequirements::{
          , requests = Some
              [ { mapKey = "storage", mapValue = values.neo4j.volumes.data.storageSize } ]
          }
        }
      }

let logVolumeClaim =
      kubernetes.PersistentVolumeClaim::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some values.neo4j.volumes.logs.name
        , namespace = Some C.polarNamespace
        }
      , spec = Some kubernetes.PersistentVolumeClaimSpec::{
        , accessModes      = Some [ "ReadWriteOnce" ]
        , storageClassName = values.neo4j.volumes.logs.storageClassName
        , resources        = Some kubernetes.VolumeResourceRequirements::{
          , requests = Some
              [ { mapKey = "storage", mapValue = values.neo4j.volumes.logs.storageSize } ]
          }
        }
      }

-- -------------------------------------------------------------------------
-- Volumes
-- -------------------------------------------------------------------------

let saTokenVolume =
      functions.makeSaTokenVolume
        neo4jSaTokenVolume
        neo4jCertClientConfig.audience
        certTokenExpiry

let certEmptyDir =
      kubernetes.Volume::{
      , name     = neo4jCertVolume
      , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
      }

let certInitScriptVol =
      kubernetes.Volume::{
      , name      = certInitScriptVolume
      , configMap = Some kubernetes.ConfigMapVolumeSource::{
        , name = Some certInitScriptCmName
        }
      }

let neo4jInitScriptVol =
      kubernetes.Volume::{
      , name      = neo4jInitScriptVolume
      , configMap = Some kubernetes.ConfigMapVolumeSource::{
        , name = Some neo4jInitScriptCmName
        }
      }

-- -------------------------------------------------------------------------
-- Init containers
-- -------------------------------------------------------------------------

-- 1. Cert bootstrap — polar-init.nu mounted at /scripts/init.nu
let certInitContainer =
      ( functions.makeNuInitContainer
          nuInitImage
          neo4jCertClientConfig
          neo4jSaTokenVolume
          neo4jCertVolume
          certInitScriptVolume
          dropAllCapSecurityContext
      ) // { imagePullPolicy = Some values.imagePullPolicy }

-- 2. Neo4j setup — runs as root so it can chown for uid 7474.
--    Executes the shell script directly rather than via polar-nu-init.
let neo4jInitContainer =
      kubernetes.Container::{
      , name            = "neo4j-init"
      , image           = Some "docker.io/alpine:3.14.0"
      , imagePullPolicy = Some values.imagePullPolicy
      , command         = Some [ "/bin/sh", "-c" ]
      , args            = Some [ neo4jInitScript ]
      , env = Some
          [ kubernetes.EnvVar::{ name = "POLAR_CERT_DIR", value = Some neo4jCertDir }
          , kubernetes.EnvVar::{
            , name      = "NEO4J_AUTH"
            , valueFrom = Some kubernetes.EnvVarSource::{
              , secretKeyRef = Some neo4jSecretSelector
              }
            }
          ]
      , volumeMounts = Some
          [ kubernetes.VolumeMount::{ name = neo4jInitScriptVolume,  mountPath = "/scripts",              readOnly = Some True }
          , kubernetes.VolumeMount::{ name = neo4jConfigmapName,     mountPath = "/config",               readOnly = Some True }
          , kubernetes.VolumeMount::{ name = values.neo4j.configVolume, mountPath = "/var/lib/neo4j/conf" }
          , kubernetes.VolumeMount::{ name = neo4jCertVolume,        mountPath = neo4jCertDir }
          ]
      }

-- -------------------------------------------------------------------------
-- Main container
-- -------------------------------------------------------------------------

let baseVolumeMounts =
      [ kubernetes.VolumeMount::{ name = values.neo4j.configVolume,      mountPath = "/var/lib/neo4j/conf" }
      , kubernetes.VolumeMount::{ name = values.neo4j.volumes.data.name, mountPath = values.neo4j.volumes.data.mountPath }
      , kubernetes.VolumeMount::{ name = values.neo4j.volumes.logs.name, mountPath = values.neo4j.volumes.logs.mountPath }
      , kubernetes.VolumeMount::{ name = "tmp",                          mountPath = "/tmp" }
      ]

let tlsVolumeMounts =
      if values.neo4j.enableTls
      then [ kubernetes.VolumeMount::{ name = neo4jCertVolume, mountPath = neo4jCertDir } ]
      else [] : List kubernetes.VolumeMount.Type

let neo4jContainer =
      kubernetes.Container::{
      , name            = values.neo4j.name
      , image           = Some values.neo4j.image
      , imagePullPolicy = Some values.imagePullPolicy
      , securityContext = Some neo4jSecurityContext
      , env = Some
          [ kubernetes.EnvVar::{
            , name      = "NEO4J_AUTH"
            , valueFrom = Some kubernetes.EnvVarSource::{
              , secretKeyRef = Some neo4jSecretSelector
              }
            }
          ]
      , ports = Some
          (   [ kubernetes.ContainerPort::{ containerPort = values.neo4j.ports.http }
              , kubernetes.ContainerPort::{ containerPort = values.neo4j.ports.bolt }
              ]
            # ( if values.neo4j.enableTls
                then [ kubernetes.ContainerPort::{ containerPort = values.neo4j.ports.https } ]
                else [] : List kubernetes.ContainerPort.Type
              )
          )
      , volumeMounts = Some (baseVolumeMounts # tlsVolumeMounts)
      }

-- -------------------------------------------------------------------------
-- Pod spec
-- -------------------------------------------------------------------------

let allVolumes =
      [ kubernetes.Volume::{
        , name                  = values.neo4j.volumes.data.name
        , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = values.neo4j.volumes.data.name }
        }
      , kubernetes.Volume::{
        , name                  = values.neo4j.volumes.logs.name
        , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = values.neo4j.volumes.logs.name }
        }
      , kubernetes.Volume::{
        , name      = values.neo4j.config.name
        , configMap = Some kubernetes.ConfigMapVolumeSource::{
          , name  = Some values.neo4j.config.name
          , items = Some [ kubernetes.KeyToPath::{ key = "neo4j.conf", path = "neo4j.conf" } ]
          }
        }
      , kubernetes.Volume::{ name = values.neo4j.configVolume, emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
      , kubernetes.Volume::{ name = "tmp",                     emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
      , saTokenVolume
      , certEmptyDir
      , certInitScriptVol
      , neo4jInitScriptVol
      ]

let podSpec =
      kubernetes.PodSpec::{
      , serviceAccountName = Some "neo4j-sa"
      -- Disabled so neo4j doesn't pick up k8s-injected env vars prefixed
      -- with the service name "NEO4J_", which collide with its own config vars.
      , enableServiceLinks = Some False
      , initContainers     = Some [ certInitContainer, neo4jInitContainer ]
      , containers         = [ neo4jContainer ]
      , securityContext    = Some kubernetes.PodSecurityContext::{
        , fsGroup             = Some 7474
        , fsGroupChangePolicy = Some "OnRootMismatch"
        }
      , volumes = Some allVolumes
      }

-- -------------------------------------------------------------------------
-- Service
-- -------------------------------------------------------------------------

let service =
      kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some values.neo4jServiceName
        , namespace = Some C.polarNamespace
        }
      , spec = Some kubernetes.ServiceSpec::{
        , selector = Some (toMap { name = values.neo4j.name })
        , type     = Some (if values.neo4j.enableTls then "LoadBalancer" else "NodePort")
        , ports    = Some
            ( if values.neo4j.enableTls
              then
                [ kubernetes.ServicePort::{ name = Some "https-ui", protocol = Some "TCP", targetPort = Some (kubernetes.NatOrString.Nat values.neo4j.ports.https), port = values.neo4j.ports.https }
                , kubernetes.ServicePort::{ name = Some "bolt",     protocol = Some "TCP", targetPort = Some (kubernetes.NatOrString.Nat values.neo4j.ports.bolt),  port = values.neo4j.ports.bolt  }
                ]
              else
                [ kubernetes.ServicePort::{ name = Some "http-ui", protocol = Some "TCP", targetPort = Some (kubernetes.NatOrString.Nat values.neo4j.ports.http), port = values.neo4j.ports.http }
                , kubernetes.ServicePort::{ name = Some "bolt",    protocol = Some "TCP", targetPort = Some (kubernetes.NatOrString.Nat values.neo4j.ports.bolt), port = values.neo4j.ports.bolt }
                ]
            )
        }
      }

-- -------------------------------------------------------------------------
-- StatefulSet
-- -------------------------------------------------------------------------

let statefulSet =
      kubernetes.StatefulSet::{
      , apiVersion = "apps/v1"
      , kind       = "StatefulSet"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some values.neo4j.name
        , namespace = Some C.polarNamespace
        }
      , spec = Some kubernetes.StatefulSetSpec::{
        , selector    = kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = values.neo4j.name }) }
        , serviceName = Some values.neo4jServiceName
        , replicas    = Some 1
        , template    = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , name   = Some values.neo4j.name
            , labels = Some [ { mapKey = "name", mapValue = values.neo4j.name } ]
            }
          , spec = Some podSpec
          }
        }
      }

-- -------------------------------------------------------------------------
-- Resource list
-- -------------------------------------------------------------------------

in  [ kubernetes.Resource.ServiceAccount        neo4jServiceAccount
    , kubernetes.Resource.ConfigMap             neo4jConfigMap
    , kubernetes.Resource.ConfigMap             certInitScriptConfigMap
    , kubernetes.Resource.ConfigMap             neo4jInitScriptConfigMap
    , kubernetes.Resource.Secret                neo4jSecret
    , kubernetes.Resource.PersistentVolumeClaim dataVolumeClaim
    , kubernetes.Resource.PersistentVolumeClaim logVolumeClaim
    , kubernetes.Resource.Service               service
    , kubernetes.Resource.StatefulSet           statefulSet
    ]
