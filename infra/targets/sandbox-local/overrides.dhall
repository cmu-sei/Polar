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

-- infra/targets/sandbox-local/overrides.dhall
--
-- Minimal target value overrides, scoped per chart.
-- Covers: neo4j, cassini, cert-issuer, jaeger.
-- Each chart's main.nu merges: values.dhall // overrides.<chart>
--
-- Image references are prefixed with the registry as the nodes name it, which
-- render.nu resolves once (infra/registry.nu: POLAR_NODE_REGISTRY, then the
-- target's cluster.registry, then config/cluster.nuon; 10.0.2.2:5000 by default,
-- u7s's slirp4netns gateway to the host, where `just registry-up` runs a
-- registry container on port 5000) and hands to the charts in the environment. See
-- `just cluster-load-all`/`cluster-load-minimal`, which push built
-- images there via skopeo rather than importing directly into a
-- specific cluster's containerd -- this works with any Kubernetes
-- distribution whose nodes can reach the registry, not just u7s.

let registry = (env:POLAR_NODE_REGISTRY as Text) ++ "/"

let IdentityStrategy = ../../schema/identity-strategy.dhall

let CaProvisioning = ../../schema/ca-provisioning.dhall

in
{ namespaces  = {=}
, storage     = {=}
, certManager = {=}
, jaeger =
  { image       = registry ++ "jaeger:2.13.0"
  , serviceType = "NodePort"
  }
, ace =
  { image            = registry ++ "ace-viz:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  }
, neo4j =
  { image              = registry ++ "neo4j:5.26.2"
  , certIssuerUrl      = "https://cert-issuer.polar.svc.cluster.local:8443"
  , certIssuerAudience = "polar-cert-issuer-neo4j.local"
  , nuInitImage        = registry ++ "polar-nu-init:latest"
  , neo4jSans          =
    [ "neo4j"
    , "polar-neo4j"
    , "polar-db-svc"
    , "polar-db-svc.polar-graph"
    , "polar-db-svc.polar-graph.svc.cluster.local"
    , "localhost"
    ]
  }
, cassini =
  { image           = registry ++ "cassini:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  }
, certIssuer =
  { image            = registry ++ "cert-issuer:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  , issuers =
    [ { issuerUrl             = "https://kubernetes.default.svc.cluster.local"
      , audience              = ["polar-cert-issuer.local", "polar-cert-issuer-neo4j.local"]
      , jwksUri               = Some "https://kubernetes.default.svc.cluster.local/openid/v1/jwks"
      , workloadIdentityClaim = "sub"
      , instanceBindingClaim  = "kubernetes.io/pod/uid"
      , identityStrategy      = IdentityStrategy.KubernetesServiceAccount { clusterDomain = "cluster.local" }
      , identityScopeClaim    = None Text
      , allowedAlgorithms     = ["RS256", "ES256", "EdDSA"]
      , jwksCacheTtlMinSecs   = 30
      , jwksCacheTtlMaxSecs   = 3600
      }
    -- Keycloak (human auth). issuerUrl matches the iss claim real
    -- tokens actually carry -- which, since Keycloak runs with
    -- --hostname-strict=false, depends on how the user reached it to
    -- log in. For sandbox-local that is the port-forward at
    -- localhost:8080 (see just port-forwards-up), confirmed against
    -- real tokens during testing. jwksUri is unrelated to that -- it
    -- is the address cert-issuer itself, running in-cluster, actually
    -- fetches signing keys from, so it uses the real in-cluster
    -- Service DNS name rather than localhost.
    , { issuerUrl             = "http://localhost:8080/realms/polar"
      , audience              = ["polar-cert-issuer.local"]
      , jwksUri               = Some "http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar/protocol/openid-connect/certs"
      , workloadIdentityClaim = "preferred_username"
      , instanceBindingClaim  = "sid"
      , identityStrategy      = IdentityStrategy.SuffixedClaim
      , identityScopeClaim    = Some "identity_scope"
      , allowedAlgorithms     = ["RS256"]
      , jwksCacheTtlMinSecs   = 30
      , jwksCacheTtlMaxSecs   = 3600
      }
    ]
  , serverLifetimeSecs = 43200
  -- The CA is created by `polar-admin-grant bootstrap-ca` with the cluster
  -- admin's own kubeconfig and mounted from a Secret. Callers verify against
  -- the polar-ca ConfigMap (cert_issuer_ca_configmap in target.nu); render.nu
  -- refuses to render if the two disagree.
  , caProvisioning = CaProvisioning.Provisioned
  , identityLifetimeOverrides =
    [ { identity = "graph.polar-graph.serviceaccount.cluster.local", lifetimeSecs = 31536000 } ]
  }
, postgres =
  { image = registry ++ "postgres:16"
  }
, keycloak =
  { image = registry ++ "keycloak:26.7.1"
  }
}
