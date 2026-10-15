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

let kubernetes = ../../types/kubernetes.dhall

let Constants = ../../types/lib-constants.dhall
let functions = ../../types/functions.dhall
let values = ../values.dhall

let namespace =
      kubernetes.Namespace::{
      , apiVersion = "v1"
      , kind = "Namespace"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some Constants.polarNamespace
        }
      }

let neo4jCredentialSecret =
      kubernetes.Secret::{
      , apiVersion = "v1"
      , kind = "Secret"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "polar-graph-pw"
        , namespace = Some Constants.polarNamespace
        }
      , stringData = Some
        [ { mapKey = "secret"
          , mapValue = env:GRAPH_PASSWORD as Text
          }
        ]
      , type = Some "Opaque"
      }

let ociSecret = env:DOCKER_AUTH_JSON as Text

-- =============================================================================
-- Init script ConfigMap — emitted once, mounted into every pod that leverages mTLS
-- =============================================================================

let polarInitScript = ../../../../scripts/polar-init.nu as Text

let agentInitScriptConfigMap =
    functions.makeNuInitScript
        Constants.initScriptConfigMapName
        polarInitScript

in  [ kubernetes.Resource.Namespace namespace
    , kubernetes.Resource.Secret neo4jCredentialSecret
    , kubernetes.Resource.ConfigMap agentInitScriptConfigMap
    ]
