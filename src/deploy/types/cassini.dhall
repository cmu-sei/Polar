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

-- types/cassini.dhall
{-
  Describes the full configuration needed to deploy Cassini, the mTLS
  message broker at the center of the Polar system.

  Cassini is the only component that acts as a TLS *server* — all agents
  connect to it as clients. Its cert is issued by the cert-client at pod
  startup using a projected SA token, exactly like agent client certs, but
  with --cert-type server so the issued cert carries serverAuth EKU.

  Agents verify Cassini's server cert against the shared CA. Cassini
  verifies agent client certs against the same CA via WebPkiClientVerifier.
  One CA, one trust anchor for the whole system.
-}
let kubernetes = ./kubernetes.dhall
let constants = ./lib-constants.dhall
let Agents = ./agents.dhall

let CassiniTlsConfig =
      { ca_cert_path : Text, server_cert_path : Text, server_key_path : Text }

let Cassini =
      { name : Text
      , image : Text
      , ports : { http : Natural, tcp : Natural }
      , serviceAccountName : Text
      , certClient : Agents.CertClientConfig.Type
      , tls : CassiniTlsConfig
      }

let defaults =
      { name = "cassini"
      , ports = { http = constants.cassiniHttpPort, tcp = constants.cassiniTcpPort }
      , tls =
        { ca_cert_path = constants.certPaths.ca
        , server_cert_path = constants.certPaths.cert
        , server_key_path = constants.certPaths.key
        }
        , serviceAccountName = "cassini"   -- was "cassini-sa"
      }

in  { Type = Cassini, defaults, CassiniTlsConfig }
