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

let kubernetes = ./kubernetes.dhall
let constants = ./lib-constants.dhall
let PolarNamespace = constants.polarNamespace

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

let CertClientConfig =
      { Type =
          { sa_token_path : Text
          , cert_issuer_url : Text
          , audience : Text
          , cert_dir : Text
          , cert_type : Text
          , key_algorithm : Text
          , extra_sans : Optional Text
          }
      , default =
        { sa_token_path = "/home/polar/sa-token/token"
        , cert_issuer_url =
            "http://cert-issuer.${PolarNamespace}.svc.cluster.local:8443"
        , audience = "polar-cert-issuer.local"
        , cert_dir = constants.certDir
        , cert_type = "client"
        , key_algorithm = "ecdsa-p256"
        , extra_sans = None Text
        }
      }

let PolarAgent =
      { name : Text
      , image : Text
      , certClient : CertClientConfig.Type
      , tls : ClientTlsConfig
      }

let GraphProcessor = PolarAgent //\\ { graph : GraphConfig }

let WithConfig = PolarAgent //\\ { config : Text }

let WithServiceAccount = { serviceAccountName : Text, secretName : Text }

let CertIssuer =
      { name : Text, image : Text, config : Text, serviceAccountName : Text }

let GitConsumer = GraphProcessor

let ProvenanceLinker = GraphProcessor

let KubeConsumer = GraphProcessor

let GitlabConsumer = GraphProcessor

let GitObserver = WithConfig

let BuildOrchestrator = WithConfig //\\ WithServiceAccount

let BuildProcessor = GraphProcessor

let KubeObserver = PolarAgent //\\ WithServiceAccount

let OciResolver = PolarAgent

let GitlabObserver =
          PolarAgent
      //\\  { baseIntervalSecs : Natural
            , maxBackoffSecs : Natural
            , endpoint : Text
            , token : Optional Text
            }

let HttpCredential = { username : Text, token : Text }

let HostCredentialConfig = { http : Optional HttpCredential }

let StaticCredentialConfig =
      { hosts : List { mapKey : Text, mapValue : HostCredentialConfig } }

in  { ClientTlsConfig
    , GraphConfig
    , CertClientConfig
    , PolarAgent
    , GraphProcessor
    , WithConfig
    , WithServiceAccount
    , CertIssuer
    , GitlabObserver
    , GitlabConsumer
    , KubeObserver
    , KubeConsumer
    , OciResolver
    , StaticCredentialConfig
    , GitObserver
    , GitConsumer
    , BuildProcessor
    }
