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

-- infra/layers/2-services/neo4j/pvcs.dhall
--
-- Neo4j PersistentVolumeClaims: data and logs.
-- The certs PVC has been removed — TLS certs are now issued by the
-- cert-issuer and written to an emptyDir by the polar-init init container.

let kubernetes = ../../../schema/kubernetes.dhall

let VolumeSpec =
      { name             : Text
      , storageClassName : Optional Text
      , storageSize      : Text
      , mountPath        : Text
      }

let makePVC =
      \(namespace : Text) ->
      \(v : VolumeSpec) ->
        kubernetes.PersistentVolumeClaim::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some v.name
          , namespace = Some namespace
          }
        , spec = Some kubernetes.PersistentVolumeClaimSpec::{
          , accessModes = Some [ "ReadWriteOnce" ]
          , resources   = Some kubernetes.VolumeResourceRequirements::{
            , requests = Some
              [ { mapKey = "storage", mapValue = v.storageSize } ]
            }
          , storageClassName = v.storageClassName
          }
        }

let render =
      \(v :
          { namespace : Text
          , volumes   :
            { data : VolumeSpec
            , logs : VolumeSpec
            , certs : VolumeSpec
            }
          }
      ) ->
        [ kubernetes.Resource.PersistentVolumeClaim (makePVC v.namespace v.volumes.data)
        , kubernetes.Resource.PersistentVolumeClaim (makePVC v.namespace v.volumes.logs)
        , kubernetes.Resource.PersistentVolumeClaim (makePVC v.namespace v.volumes.certs)
        ]

in render
