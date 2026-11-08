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

-- infra/layers/2-services/cert-issuer/deployment.dhall

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall
let CaProvisioning = ../../../schema/ca-provisioning.dhall

let render =
      \(v :
          { name            : Text
          , image           : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , port            : Natural
          , caVolumeName    : Text
          , caCertPath      : Text
          , caKeyPath       : Text
          , caProvisioning  : CaProvisioning
          -- The audience its own projected token is for: the one its Kubernetes issuer expects.
          , certIssuerAudience : Text
          }
      ) ->
        -- Provisioned: mount the operator-created Secret read-only,
        -- with fsGroup so the container's non-root user can read a key
        -- the kubelet writes as group-owned. SelfBootstrap: the
        -- existing PVC, writable, no fsGroup needed since cert-issuer
        -- itself creates the files.
        let caVolume =
              merge
                { Provisioned =
                    k8s.Volume::{
                    , name   = v.caVolumeName
                    , secret = Some k8s.SecretVolumeSource::{
                      , secretName  = Some "cert-issuer-ca"
                       -- Paths are relative to the mount at /home/polar and
                       -- must land where values.dhall's caCertPath/caKeyPath
                       -- point (/home/polar/ca/...). In SelfBootstrap mode the
                       -- ca/ directory is created by cert-issuer itself on the
                       -- PVC; here nothing creates it, so the Secret does.
                       , items       = Some
                         [ k8s.KeyToPath::{ key = "ca.crt", path = "ca/ca.crt" }
                         , k8s.KeyToPath::{ key = "ca.key", path = "ca/ca.key" }
                         ]
                      , defaultMode = Some 288 -- 0440
                      }
                    }
                , SelfBootstrap =
                    k8s.Volume::{
                    , name                  = v.caVolumeName
                    , persistentVolumeClaim = Some k8s.PersistentVolumeClaimVolumeSource::{ claimName = "${v.name}-ca" }
                    }
                }
                v.caProvisioning
        let podSecurityContext =
              merge
                { Provisioned =
                    Some k8s.PodSecurityContext::{ fsGroup = Some 1000 }
                , SelfBootstrap = None k8s.PodSecurityContext.Type
                }
                v.caProvisioning

        in  k8s.Deployment::{
        , metadata = k8s.ObjectMeta::{
          , name      = Some v.name
          , namespace = Some Constants.PolarNamespace
          }
        , spec = Some k8s.DeploymentSpec::{
          , selector = k8s.LabelSelector::{ matchLabels = Some (toMap { name = v.name }) }
          , replicas = Some 1
          , template = k8s.PodTemplateSpec::{
            , metadata = Some k8s.ObjectMeta::{
              , name   = Some v.name
              , labels = Some [ { mapKey = "name", mapValue = v.name } ]
              }
            , spec = Some k8s.PodSpec::{
              , serviceAccountName = Some v.name
              , automountServiceAccountToken = Some True
              , securityContext    = podSecurityContext
              , imagePullSecrets = Some v.imagePullSecrets
              -- The builder's drain budget (POLAR_SHUTDOWN_BUDGET_SECS) plus a margin, so Kubernetes does not kill the pod
              -- before its own watchdog has ended it.
              , terminationGracePeriodSeconds = Some 30
              , volumes = Some
                [ caVolume
                -- Its own token, on which it signs itself its client certificate through its own floor.
                , Constants.saTokenVolume v.certIssuerAudience
                -- Its client certificate and key, written at start, read by its Cassini client.
                , Constants.certEmptyDirVolume
                , k8s.Volume::{
                  , name     = "polar-health"
                  , emptyDir = Some k8s.EmptyDirVolumeSource::{=}
                  }
                ]
              , containers =
                [ k8s.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  -- Its declared settings (src/agents/cert-issuer/server/src/settings.rs), one key each.
                  , envFrom = Some
                    [ k8s.EnvFromSource::{
                      , configMapRef = Some k8s.ConfigMapEnvSource::{ name = Some "${v.name}-config" }
                      }
                    ]
                  , env = Some
                    [ k8s.EnvVar::{ name = "CERT_ISSUER_OWN_TOKEN", value = Some Constants.saTokenPath }
                    , k8s.EnvVar::{
                      , name  = "RUST_LOG"
                      , value = Some "info,cert_issuer=debug"
                      }
                    , k8s.EnvVar::{
                      , name  = "KUBERNETES_CA_CERT"
                      , value = Some "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
                      }
                    , k8s.EnvVar::{
                      , name  = "JAEGER_ENABLE_TRACING"
                      , value = Some "true"
                      }
                    , k8s.EnvVar::{
                      , name  = "JAEGER_OTLP_ENDPOINT"
                      , value = Some "http://jaeger-svc.polar.svc.cluster.local:4318/v1/traces"
                      }
                    -- Its Cassini client: it speaks over Cassini once Cassini is up, and does not depend on it.
                    , k8s.EnvVar::{ name = "BROKER_ADDR", value = Some Constants.cassiniAddr }
                    , k8s.EnvVar::{ name = "CASSINI_SERVER_NAME", value = Some Constants.cassiniDNSName }
                    , k8s.EnvVar::{ name = "TLS_CLIENT_CERT", value = Some "${Constants.tlsPath}/cert.pem" }
                    , k8s.EnvVar::{ name = "TLS_CLIENT_KEY", value = Some "${Constants.tlsPath}/key.pem" }
                    -- It writes the CA it signs under here, beside its own certificate: the CA's own file is on a
                    -- read-only Secret.
                    , k8s.EnvVar::{ name = "TLS_CA_CERT", value = Some "${Constants.tlsPath}/ca.pem" }
                    , k8s.EnvVar::{ name = "POLAR_CAPABILITIES", value = Some "cassini" }
                    -- The health actor and the polar-healthcheck probe, as the catalog's kinds set them
                    -- (infra/config/kinds.nuon, `health`).
                    , k8s.EnvVar::{
                      , name  = "POLAR_HEALTH_FILE"
                      , value = Some "/var/run/polar-health/polar-health.json"
                      }
                    , k8s.EnvVar::{ name = "POLAR_HEALTH_CERTS", value = Some "${Constants.tlsPath}/cert.pem" }
                    , k8s.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS", value = Some "60" }
                    , k8s.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS", value = Some "30" }
                    , k8s.EnvVar::{ name = "POLAR_HEALTH_REJUVENATION_SECS", value = Some "300" }
                    , k8s.EnvVar::{ name = "POLAR_HEALTH_STALE_SECS", value = Some "90" }
                    , k8s.EnvVar::{ name = "POLAR_SHUTDOWN_BUDGET_SECS", value = Some "20" }
                    ]
                  , ports = Some
                    [ k8s.ContainerPort::{ containerPort = v.port, name = Some "http" } ]
                  , volumeMounts = Some
                    [ k8s.VolumeMount::{
                      , name      = v.caVolumeName
                      , mountPath = "/home/polar"
                      }
                    , k8s.VolumeMount::{
                      , name      = Constants.saTokenVolumeName
                      , mountPath = "/workspace"
                      , readOnly  = Some True
                      }
                    , Constants.certVolumeMount
                    , k8s.VolumeMount::{
                      , name      = "polar-health"
                      , mountPath = "/var/run/polar-health"
                      }
                    ]
                  , livenessProbe = Some k8s.Probe::{
                    , exec                = Some { command = Some [ "polar-healthcheck" ] }
                    , initialDelaySeconds = Some 150
                    , periodSeconds       = Some 30
                    , failureThreshold    = Some 2
                    , timeoutSeconds      = Some 5
                    }
                  }
                ]
              }
            }
          }
        }

in render
