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

{-
  local/cassini.dhall — Cassini broker deployment for local cluster development.

  Uses the polar-nu-init image to run the cert bootstrap script at pod
  startup. The nushell script is mounted from a ConfigMap at /scripts/init.nu.
  The nu-init entrypoint checks for the script, executes it, and exits.
  Cassini starts only after the init container exits zero.
-}

let kubernetes = ../../types/kubernetes.dhall
let C          = ../../types/lib-constants.dhall
let Polar      = ../../types/package.dhall
let functions  = Polar.functions
let values     = ../values.dhall

let cassini = values.cassini

-- -------------------------------------------------------------------------
-- Deployment-local constants
-- -------------------------------------------------------------------------

let nuInitImage = values.nuInitImage

let saTokenVolumeName   = C.saTokenVolumeName
let certVolumeName       = C.certVolumeName
let initScriptVolumeName = C.initScriptVolumeName
let certTokenExpiry      =  C.certTokenExpiry

let initScriptConfigMapName = C.initScriptConfigMapName

-- ClusterIP service wiring for Cassini. The service name forms part of
-- the in-cluster DNS name that agents dial; it must match C.cassiniServiceName.
let cassiniServiceType = "ClusterIP"

let dropAllCapSecurityContext =
      kubernetes.SecurityContext::{
      , runAsGroup   = Some 1000
      , runAsNonRoot = Some True
      , runAsUser    = Some 1000
      , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
      }

-- -------------------------------------------------------------------------
-- Service
-- -------------------------------------------------------------------------

let cassiniService =
      kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some C.cassiniServiceName
        , namespace = Some C.polarNamespace
        }
      , spec = Some kubernetes.ServiceSpec::{
        , selector = Some (toMap { name = cassini.name })
        , type     = Some cassiniServiceType
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

-- -------------------------------------------------------------------------
-- Service account
-- -------------------------------------------------------------------------

let cassiniServiceAccount =
      kubernetes.ServiceAccount::{
      , apiVersion = "v1"
      , kind       = "ServiceAccount"
      , metadata   = kubernetes.ObjectMeta::{
        , name      = Some cassini.serviceAccountName
        , namespace = Some C.polarNamespace
        }
      , automountServiceAccountToken = Some False
      }

-- -------------------------------------------------------------------------
-- Volumes
-- -------------------------------------------------------------------------

let certVolumes =
      functions.makeCertVolumes
        saTokenVolumeName
        certVolumeName
        cassini.certClient.audience
        certTokenExpiry

let scriptVolume =
      kubernetes.Volume::{
      , name      = initScriptVolumeName
      , configMap = Some kubernetes.ConfigMapVolumeSource::{
        , name = Some initScriptConfigMapName
        }
      }

let allVolumes = certVolumes # [ scriptVolume ]

-- -------------------------------------------------------------------------
-- Init container
-- -------------------------------------------------------------------------

let certInitContainer =
      ( functions.makeNuInitContainer
          nuInitImage
          (cassini.certClient // { cert_type = "server" })
          saTokenVolumeName
          certVolumeName
          initScriptVolumeName
          dropAllCapSecurityContext
      ) // { imagePullPolicy = Some values.imagePullPolicy }

-- -------------------------------------------------------------------------
-- Cassini container
-- -------------------------------------------------------------------------

let environment =
      [ kubernetes.EnvVar::{ name = "TLS_CA_CERT",          value = Some cassini.tls.ca_cert_path }
      , kubernetes.EnvVar::{ name = "TLS_SERVER_CERT_CHAIN", value = Some cassini.tls.server_cert_path }
      , kubernetes.EnvVar::{ name = "TLS_SERVER_KEY",        value = Some cassini.tls.server_key_path }
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
      , securityContext = Some dropAllCapSecurityContext
      , env             = Some environment
      , ports           = Some
        [ kubernetes.ContainerPort::{ containerPort = cassini.ports.tcp }
        , kubernetes.ContainerPort::{ containerPort = cassini.ports.http }
        ]
      , volumeMounts = Some
          ( functions.makeAgentCertMount
              certVolumeName
              cassini.certClient.cert_dir
          )
      }

-- -------------------------------------------------------------------------
-- Deployment
-- -------------------------------------------------------------------------

let deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name      = Some cassini.name
        , namespace = Some C.polarNamespace
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
            , initContainers     = Some [ certInitContainer ]
            , containers         = [ cassiniContainer ]
            , volumes            = Some allVolumes
            }
          }
        }
      }

in  [ kubernetes.Resource.ServiceAccount cassiniServiceAccount
    , kubernetes.Resource.Service         cassiniService
    , kubernetes.Resource.Deployment      deployment
    ]
