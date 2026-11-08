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

-- Example cert issuer config for a local development cluster.
--
-- Render to JSON for the binary to consume:
--   dhall-to-json --file dev.dhall > dev.json
--   CERT_ISSUER_CONFIG=dev.json ./target/debug/cert-issuer
--


let CertIssuer = ./cert-issuer.dhall
let cluster_name = "dev"

in  CertIssuer.ServiceConfig::{
    , bind_addr = "127.0.0.1:8443"
    , issuer = CertIssuer.IssuerConfig::{
      , -- The Kubernetes API server's OIDC issuer URL. For
        -- in-cluster clients this is normally
        -- https://kubernetes.default.svc; for local development
        -- you'll typically run a small OIDC stub instead.
        issuer = "http://localhost:8080"
      , -- Tokens must request this audience via the projected SA
        -- token's `audiences` field in the pod spec (or via your
        -- OIDC stub's claims). Every pod that wants to attest must
        -- claim the same value.
        audience = ["polar-cert-issuer.${cluster_name}"]
      , jwks_uri = Some "http://localhost:8080/jwks.json"
      }
    , ca = CertIssuer.CaConfig::{
      , ca_cert_path = "/home/polar/ca.crt"
      , ca_key_path = "/home/polar/ca.key"
      , -- Slightly shorter lifetime in dev to surface
        -- restart-driven renewal issues quickly. Production
        -- typically uses the 1-hour default.
        default_lifetime = CertIssuer.minutes 30
      }
    }
