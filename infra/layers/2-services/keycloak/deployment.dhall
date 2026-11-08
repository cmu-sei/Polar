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

-- infra/layers/2-services/keycloak/deployment.dhall
--
-- Keycloak Deployment. Stateless -- all real state lives in Postgres.
-- Started in production mode (not start-dev) against a real Postgres
-- backing store, so realm/user/client data survives restarts. Keycloaks
-- own listener runs permissively (plain HTTP, non-strict hostname) for
-- now -- TLS/hostname hardening for the login UI itself is a separate,
-- later piece of work, not solved here.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v :
          { name              : Text
          , image             : Text
          , imagePullPolicy   : Text
          , ports             : { http : Natural }
          , dbHost            : Text
          , dbName            : Text
          , dbUser            : Text
          , adminUserSecret   : { name : Text, key : Text }
          , adminPasswordSecret : { name : Text, key : Text }
          , dbPasswordSecret  : { name : Text, key : Text }
          }
      ) ->
        kubernetes.Deployment::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some v.name
          , namespace = Some Constants.AuthNamespace
          }
        , spec = Some kubernetes.DeploymentSpec::{
          , selector = kubernetes.LabelSelector::{
            , matchLabels = Some (toMap { name = v.name })
            }
          , replicas = Some 1
          , template = kubernetes.PodTemplateSpec::{
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
                  , args            = Some
                    [ "start"
                    , "--http-enabled=true"
                    , "--hostname-strict=false"
                    , "--db=postgres"
                    ]
                  , env             = Some
                    [ kubernetes.EnvVar::{
                      , name      = "KC_BOOTSTRAP_ADMIN_USERNAME"
                      , valueFrom = Some kubernetes.EnvVarSource::{
                        , secretKeyRef = Some kubernetes.SecretKeySelector::{
                          , name = Some v.adminUserSecret.name
                          , key  = v.adminUserSecret.key
                          }
                        }
                      }
                    , kubernetes.EnvVar::{
                      , name      = "KC_BOOTSTRAP_ADMIN_PASSWORD"
                      , valueFrom = Some kubernetes.EnvVarSource::{
                        , secretKeyRef = Some kubernetes.SecretKeySelector::{
                          , name = Some v.adminPasswordSecret.name
                          , key  = v.adminPasswordSecret.key
                          }
                        }
                      }
                    , kubernetes.EnvVar::{
                      , name  = "KC_DB_URL"
                      , value = Some "jdbc:postgresql://${v.dbHost}.${Constants.AuthNamespace}.svc.cluster.local:5432/${v.dbName}"
                      }
                    , kubernetes.EnvVar::{ name = "KC_DB_USERNAME", value = Some v.dbUser }
                    , kubernetes.EnvVar::{
                      , name      = "KC_DB_PASSWORD"
                      , valueFrom = Some kubernetes.EnvVarSource::{
                        , secretKeyRef = Some kubernetes.SecretKeySelector::{
                          , name = Some v.dbPasswordSecret.name
                          , key  = v.dbPasswordSecret.key
                          }
                        }
                      }
                    ]
                  , ports = Some [ kubernetes.ContainerPort::{ containerPort = v.ports.http } ]
                  }
                ]
              }
            }
          }
        }

in render
