-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

-- infra/layers/2-services/neo4j/statefulset.dhall
--
-- Neo4j StatefulSet.
-- Init containers (in order):
--   1. polar-init: requests a server TLS cert from the cert-issuer,
--      writes cert.pem, key.pem, ca.pem to the neo4j-tls emptyDir.
--   2. neo4j-init: reads certs from neo4j-tls, writes them into the
--      neo4j certificate directory structure, and copies neo4j.conf.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall
let Functions = ../../../schema/functions.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let VolumeSpec =
      { name             : Text
      , storageClassName : Optional Text
      , storageSize      : Text
      , mountPath        : Text
      }

let render =
      \(v :
          { name                : Text
          , image               : Text
          , imagePullPolicy     : Text
          , namespace           : Text
          , serviceAccountName  : Text
          , configVolume        : Text
          , credentialsObserverUrl : Text
          , polarInitImage      : Text
          , nuInitImage          : Text
          , certIssuerAudience  : Text
          , neo4jSans           : List Text
          , ports               : { http : Natural, https : Natural, bolt : Natural }
          , config              : { name : Text, path : Text }
          , volumes             : { data : VolumeSpec, logs : VolumeSpec, certs : VolumeSpec }
          }
      ) ->

        let neo4jTlsVolume =
        kubernetes.Volume::{
        , name     = "neo4j-tls"
        , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        }

        let neo4jSaTokenVolume =
        kubernetes.Volume::{
        , name = "neo4j-sa-token"
        , projected = Some kubernetes.ProjectedVolumeSource::{
          , sources = Some
            [ kubernetes.VolumeProjection::{
              , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                , path     = "token"
                , audience = Some v.certIssuerAudience
                , expirationSeconds = Some 3600
                }
              }
            ]
          }
        }

        let baseVolumeMounts =
              [ kubernetes.VolumeMount::{ name = v.configVolume,       mountPath = "/var/lib/neo4j/conf"           }
              , kubernetes.VolumeMount::{ name = v.volumes.data.name,  mountPath = v.volumes.data.mountPath        }
              , kubernetes.VolumeMount::{ name = v.volumes.logs.name,  mountPath = v.volumes.logs.mountPath        }
              , kubernetes.VolumeMount::{ name = v.volumes.certs.name, mountPath = v.volumes.certs.mountPath       }
              , kubernetes.VolumeMount::{ name = "tmp",                mountPath = "/tmp"                          }
              ]

        let initVolumeMounts =
              [ kubernetes.VolumeMount::{ name = Constants.neo4jConfigmapName, mountPath = "/config"                    }
              , kubernetes.VolumeMount::{ name = v.configVolume,               mountPath = "/var/lib/neo4j/conf"         }
              , kubernetes.VolumeMount::{ name = "neo4j-init-script",          mountPath = "/scripts"                    }
              , kubernetes.VolumeMount::{ name = v.volumes.certs.name,         mountPath = v.volumes.certs.mountPath     }
              , kubernetes.VolumeMount::{ name = "neo4j-tls",                  mountPath = "/etc/neo4j-tls", readOnly = Some True }
              ]

        let baseEnv =
              [ kubernetes.EnvVar::{
                , name      = "NEO4J_AUTH"
                , valueFrom = Some kubernetes.EnvVarSource::{
                  , secretKeyRef = Some Constants.neo4jSecret
                  }
                }
              ]

        let containerPorts =
              [ kubernetes.ContainerPort::{ containerPort = v.ports.http  }
              , kubernetes.ContainerPort::{ containerPort = v.ports.bolt  }
              , kubernetes.ContainerPort::{ containerPort = v.ports.https }
              ]

        let baseVolumes =
              [ kubernetes.Volume::{
                , name                  = v.volumes.data.name
                , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = v.volumes.data.name }
                }
              , kubernetes.Volume::{
                , name                  = v.volumes.logs.name
                , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = v.volumes.logs.name }
                }
              , kubernetes.Volume::{
                , name      = v.config.name
                , configMap = Some kubernetes.ConfigMapVolumeSource::{
                  , name  = Some v.config.name
                  , items = Some [ kubernetes.KeyToPath::{ key = "neo4j.conf", path = "neo4j.conf" } ]
                  }
                }
              , kubernetes.Volume::{
                , name                  = v.volumes.certs.name
                , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{ claimName = v.volumes.certs.name }
                }
              , kubernetes.Volume::{ name = v.configVolume, emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              , kubernetes.Volume::{ name = "tmp",          emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              , kubernetes.Volume::{
                , name      = "neo4j-init-script"
                , configMap = Some kubernetes.ConfigMapVolumeSource::{
                  , name  = Some "neo4j-init-script"
                  , items = Some [ kubernetes.KeyToPath::{ key = "init.nu", path = "init.nu" } ]
                  }
                }
              ]

        in  kubernetes.StatefulSet::{
            , apiVersion = "apps/v1"
            , kind       = "StatefulSet"
            , metadata   = kubernetes.ObjectMeta::{
              , name      = Some v.name
              , namespace = Some v.namespace
              }
            , spec = Some kubernetes.StatefulSetSpec::{
              , selector    = kubernetes.LabelSelector::{
                , matchLabels = Some (toMap { name = v.name })
                }
              , serviceName = Constants.neo4jServiceName
              , replicas    = Some 1
              , template    = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{
                  , name   = Some v.name
                  , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                  }
                , spec = Some kubernetes.PodSpec::{
                  , serviceAccountName = Some v.serviceAccountName
                  , automountServiceAccountToken = Some False
                  , securityContext = Some kubernetes.PodSecurityContext::{
                    , fsGroup             = Some 7474
                    , fsGroupChangePolicy = Some "OnRootMismatch"
                    }
                  , initContainers = Some
                    [ Functions.makePolarInitContainer
                        v.polarInitImage
                        v.imagePullPolicy
                        "neo4j-sa-token"
                        -- Not told where cert-issuer is: its server certificate comes from the
                        -- credentials agent. Neo4j is not a bootstrap identity.
                        ""
                        (Functions.credentialsAgentIssuerEnv v.credentialsObserverUrl)
                        "/workspace/token"
                        [ kubernetes.VolumeMount::{ name = "neo4j-tls", mountPath = "/etc/neo4j-tls" } ]
                        ([] : List Text)
                        -- The credentials agent does not need Neo4j to answer, so
                        -- getting the server certificate from it is no startup deadlock.
                        [ "server:/etc/neo4j-tls:ecdsa-p256:${Prelude.Text.concatSep "," v.neo4jSans}" ]
                    , kubernetes.Container::{
                      , name            = "neo4j-init"
                      , image           = Some v.nuInitImage
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some kubernetes.SecurityContext::{
                        , runAsUser    = Some 0
                        , runAsGroup   = Some 0
                        , runAsNonRoot = Some False
                        }
                      , env          = Some baseEnv
                      , volumeMounts = Some initVolumeMounts
                      }
                    , kubernetes.Container::{
                      , name            = "neo4j-cacerts"
                      , image           = Some v.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some kubernetes.SecurityContext::{
                        , runAsUser    = Some 0
                        , runAsGroup   = Some 0
                        , runAsNonRoot = Some False
                        }
                      , command = Some [ "/bin/sh", "-c" ]
                      , args = Some
                      [ "rm -f /var/lib/neo4j/conf/cacerts && /bin/keytool -import -noprompt -alias polar-internal-ca -file /var/lib/neo4j/certificates/https/trusted/ca.pem -keystore /var/lib/neo4j/conf/cacerts -storepass changeit -storetype PKCS12"
                      ]
                      , volumeMounts = Some
                        [ kubernetes.VolumeMount::{ name = v.configVolume,       mountPath = "/var/lib/neo4j/conf"        }
                        , kubernetes.VolumeMount::{ name = v.volumes.certs.name, mountPath = v.volumes.certs.mountPath     }
                        ]
                      }
                    ]
                  , containers =
                    [ kubernetes.Container::{
                      , name            = v.name
                      , image           = Some v.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some kubernetes.SecurityContext::{
                        , runAsGroup   = Some 7474
                        , runAsNonRoot = Some True
                        , runAsUser    = Some 7474
                        , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
                        }
                      , env          = Some (baseEnv)
                      , ports        = Some containerPorts
                      , volumeMounts = Some (baseVolumeMounts)
                      }
                    ]
                  , volumes = Some (baseVolumes # [ neo4jTlsVolume, neo4jSaTokenVolume ])
                  }
                }
              }
            }

in render
