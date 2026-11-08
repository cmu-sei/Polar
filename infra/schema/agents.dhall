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

let kubernetes =
      ./kubernetes.dhall
        sha256:2086df2178fdb83a08bd4ca89c951f12e4ce16d3cc1441005fcda943a028bb8c

let GraphConfig =
      { graphDB : Text
      , graphUsername : Text
      , graphPassword : kubernetes.SecretKeySelector.Type
      }

let ClientTlsConfig =
      { broker_endpoint : Text
      , server_name : Text
      , client_certificate_path : Text
      , client_key_path : Text
      , client_ca_cert_path : Text
      }

let PolarAgent = { name : Text, image : Text, tls : ClientTlsConfig }

let GraphProcessor = PolarAgent //\\ { graph : GraphConfig }

let WithServiceAccount = { serviceAccountName : Text, secretName : Text }

let GitConsumer = GraphProcessor

let GitScheduler = GraphProcessor

let ProvenanceLinker = GraphProcessor

let KubeConsumer = GraphProcessor

let GitlabConsumer = GraphProcessor

let BuildProcessor = GraphProcessor

let GitObserver = PolarAgent

let BuildOrchestrator = PolarAgent //\\ WithServiceAccount

let KubeObserver = PolarAgent //\\ WithServiceAccount

let ProvenanceResolver = PolarAgent

let GitlabObserver =
          PolarAgent
      //\\  { baseIntervalSecs : Natural
            , maxBackoffSecs : Natural
            , endpoint : Text
            , token : Optional Text
            }

in  { ClientTlsConfig
    , GraphConfig
    , PolarAgent
    , GraphProcessor
    , WithServiceAccount
    , GitlabObserver
    , GitlabConsumer
    , KubeObserver
    , KubeConsumer
    , ProvenanceLinker
    , ProvenanceResolver
    , GitObserver
    , GitConsumer
    , GitScheduler
    , BuildOrchestrator
    , BuildProcessor
    }
