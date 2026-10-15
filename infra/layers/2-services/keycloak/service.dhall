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

-- infra/layers/2-services/keycloak/service.dhall
--
-- NodePort -- same shape as jaeger: one Service satisfies both in-cluster
-- reachability (cert-issuer -> Keycloak for JWKS/discovery) and
-- host-external reachability (an admins browser reaching the login UI).

let kubernetes = ../../../schema/kubernetes.dhall
let Constants  = ../../../schema/constants.dhall

let render =
      \(v : { name : Text, serviceType : Text, ports : { http : Natural } }) ->
        kubernetes.Service::{
        , metadata = kubernetes.ObjectMeta::{
          , name      = Some "${v.name}-svc"
          , namespace = Some Constants.AuthNamespace
          }
        , spec = Some kubernetes.ServiceSpec::{
          , selector = Some (toMap { name = v.name })
          , type     = Some v.serviceType
          , ports    = Some
            [ kubernetes.ServicePort::{
              , name       = Some "http"
              , targetPort = Some (kubernetes.NatOrString.Nat v.ports.http)
              , port       = v.ports.http
              }
            ]
          }
        }

in render
