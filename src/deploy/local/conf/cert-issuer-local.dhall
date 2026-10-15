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
  conf/cert-issuer-local.dhall

  Cert issuer config for local cluster development via OrbStack or usernetes.
  The OIDC issuer is the local API server. The CA materials are bootstrapped
  by the cert issuer on first run and persisted to the configured PVC mount.

  Render to JSON before deploying:
    dhall-to-json --file conf/cert-issuer-local.dhall > conf/cert-issuer.json
-}
let CertIssuer = ../../../agents/cert-issuer/server/conf/cert-issuer.dhall

in  CertIssuer.ServiceConfig::{
    , bind_addr = "0.0.0.0:8443"
    , issuer = CertIssuer.IssuerConfig::{
      , issuer = "https://kubernetes.default.svc.cluster.local"
      , audience = [ "polar-cert-issuer.local" ]
      , jwks_uri = Some
          "https://kubernetes.default.svc.cluster.local/openid/v1/jwks"
      }
    , ca = CertIssuer.CaConfig::{
      , ca_cert_path = "/home/polar/ca/ca.crt"
      , ca_key_path = "/home/polar/ca/ca.key"
      , default_lifetime = CertIssuer.minutes 480
      }
    }
