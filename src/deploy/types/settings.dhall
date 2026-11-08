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
  types/settings.dhall — environment variables as the library's agents are given them, written once.
-}
let kubernetes = ./kubernetes.dhall

--| A variable with a value.
let setting =
      λ(name : Text) →
      λ(value : Text) →
        kubernetes.EnvVar::{ name, value = Some value }

--| A variable whose value is a key of a Secret.
let secretSetting =
      λ(name : Text) →
      λ(secretName : Text) →
      λ(key : Text) →
        kubernetes.EnvVar::{
        , name
        , valueFrom = Some kubernetes.EnvVarSource::{
          , secretKeyRef = Some kubernetes.SecretKeySelector::{ name = Some secretName, key }
          }
        }

--| The graph, as an agent that writes to it reads its settings. No certificate settings: the
--| development graph has no TLS.
let graphSettings =
      λ(namespace : Text) →
        [ setting "GRAPH_ENDPOINT" "bolt://polar-db-svc.${namespace}.svc.cluster.local:7687"
        , setting "GRAPH_DB" "neo4j"
        , setting "GRAPH_USER" "neo4j"
        , secretSetting "GRAPH_PASSWORD" "polar-graph-pw" "secret"
        ]

in  { setting, secretSetting, graphSettings }
