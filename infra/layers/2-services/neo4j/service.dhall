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

-- infra/layers/2-services/neo4j/service.dhall
--
-- Neo4j Service. TLS is always enabled via cert-issuer.

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v :
          { ports : { http : Natural, https : Natural, bolt : Natural }
          }
      ) ->
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some Constants.neo4jServiceName
          , namespace = Some Constants.GraphNamespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some (toMap { name = "polar-neo4j" })
          , type     = Some "LoadBalancer"
          , ports    = Some
            [ kubernetes.ServicePort::{
              , name       = Some "https-ui"
              , protocol   = Some "TCP"
              , targetPort = Some (kubernetes.NatOrString.Nat v.ports.https)
              , port       = v.ports.https
              }
            , kubernetes.ServicePort::{
              , name       = Some "bolt"
              , protocol   = Some "TCP"
              , targetPort = Some (kubernetes.NatOrString.Nat v.ports.bolt)
              , port       = v.ports.bolt
              }
            ]
          }
        }

in render
