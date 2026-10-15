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

-- infra/layers/2-services/postgres/statefulset.dhall
--
-- Postgres StatefulSet -- backing store for Keycloak. Real persistence,
-- not ephemeral: Keycloak realm/user/client data must survive restarts.

let kubernetes = ../../../schema/kubernetes.dhall

let render =
      \(v :
          { name             : Text
          , image            : Text
          , imagePullPolicy  : Text
          , namespace        : Text
          , dbName           : Text
          , dbUser           : Text
          , dbPasswordSecret : { name : Text, key : Text }
          }
      ) ->
        kubernetes.StatefulSet::{
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
          , serviceName = v.name
          , replicas    = Some 1
          , template    = kubernetes.PodTemplateSpec::{
            , metadata = Some kubernetes.ObjectMeta::{
              , name   = Some v.name
              , labels = Some [ { mapKey = "name", mapValue = v.name } ]
              }
            , spec = Some kubernetes.PodSpec::{
              , serviceAccountName = Some v.name
              , automountServiceAccountToken = Some False
              , containers =
                [ kubernetes.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , env             = Some
                    [ kubernetes.EnvVar::{ name = "POSTGRES_DB",   value = Some v.dbName }
                    , kubernetes.EnvVar::{ name = "POSTGRES_USER", value = Some v.dbUser }
                    , kubernetes.EnvVar::{
                      , name      = "POSTGRES_PASSWORD"
                      , valueFrom = Some kubernetes.EnvVarSource::{
                        , secretKeyRef = Some kubernetes.SecretKeySelector::{
                          , name = Some v.dbPasswordSecret.name
                          , key  = v.dbPasswordSecret.key
                          }
                        }
                      }
                    ]
                  , ports        = Some [ kubernetes.ContainerPort::{ containerPort = 5432 } ]
                  , volumeMounts = Some
                    [ kubernetes.VolumeMount::{
                      , name      = "${v.name}-data"
                      , mountPath = "/var/lib/postgresql/data"
                      }
                    ]
                  }
                ]
              , volumes = Some
                [ kubernetes.Volume::{
                  , name                  = "${v.name}-data"
                  , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                    , claimName = "${v.name}-data"
                    }
                  }
                ]
              }
            }
          }
        }

in render
