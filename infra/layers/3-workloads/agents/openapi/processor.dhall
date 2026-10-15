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

-- infra/layers/3-workloads/agents/openapi/processor.dhall
--
-- OpenAPI processor Job.
-- Reads Cassini topics and writes API spec data to the graph.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name             : Text
          , image            : Text
          , imagePullPolicy  : Text
          , imagePullSecrets : List { name : Optional Text }
          , polarInitImage   : Text
          , certIssuerUrl    : Text
          , saTokenAudience  : Text
          , neo4jBoltAddr    : Text
          }
      ) ->
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , kubernetes.Volume::{ name = Constants.neo4jClientCertVolumeName, emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              , kubernetes.Volume::{ name = "polar-health", emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              ]

        let env =
              Constants.commonClientEnv
              # functions.makeGraphEnv v.neo4jBoltAddr Constants.graphConfig Constants.graphSecretKeySelector (Some "/etc/neo4j-client-tls/ca.pem")
              # [ kubernetes.EnvVar::{ name = "POLAR_HEALTH_FILE",          value = Some "/var/run/polar-health/polar-health.json" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_CERTS",         value = Some "/etc/tls/certs/cert.pem,/etc/neo4j-client-tls/cert.pem" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_EXPIRY_SECS",   value = Some "60" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_TICK_SECS",     value = Some "30" }
                , kubernetes.EnvVar::{ name = "POLAR_HEALTH_DEP_ENDPOINTS", value = Some "${Constants.cassiniDNSName}:${Natural/show Constants.cassiniPort}:300,${Constants.neo4jDNSName}:7687:300" }
                ]

        let mounts =
              [ Constants.certVolumeMount
              , kubernetes.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath, readOnly = Some True }
              , kubernetes.VolumeMount::{ name = "polar-health", mountPath = "/var/run/polar-health" }
              ]

        let podSpec = kubernetes.PodSpec::{
              , imagePullSecrets = Some v.imagePullSecrets
              , serviceAccountName = Some v.name
              , automountServiceAccountToken = Some False
              , restartPolicy    = Some "Never"
              , volumes          = Some volumes
              , initContainers   = Some
                [ functions.makePolarInitContainer
                    v.polarInitImage
                    v.imagePullPolicy
                    Constants.saTokenVolumeName
                    v.certIssuerUrl
                    functions.noEnv
                    Constants.saTokenPath
                    [ kubernetes.VolumeMount::{ name = Constants.certVolumeName,            mountPath = Constants.tlsPath             }
                    , kubernetes.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath }
                    ]
                    ([] : List Text)
                    [ "client:${Constants.tlsPath}:ecdsa-p256:"
                    , "client:${Constants.neo4jClientCertPath}:ecdsa-p256:"
                    ]
                    False
                ]
              , containers =
                [ kubernetes.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  , env             = Some env
                  , volumeMounts    = Some mounts
                  , livenessProbe   = Some kubernetes.Probe::{
                      , exec                = Some { command = Some [ "polar-healthcheck" ] }
                      , initialDelaySeconds = Some 150
                      , periodSeconds       = Some 30
                      , failureThreshold    = Some 2
                      , timeoutSeconds      = Some 5
                      }
                  }
                ]
              }

        in  kubernetes.Job::{
            , metadata = kubernetes.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.JobSpec::{
              , backoffLimit            = Some 1000000
              , podReplacementPolicy = Some Constants.JobPodReplacementPolicy
              , parallelism             = Some 1
              , completions             = Some 1
              , ttlSecondsAfterFinished = Some 300
              , template                = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec     = Some podSpec
                }
              }
            }

in render
