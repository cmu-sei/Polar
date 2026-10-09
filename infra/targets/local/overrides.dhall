-- infra/targets/local/overrides.dhall
--
-- Local target value overrides, scoped per chart.
-- Each chart's main.nu merges: values.dhall // overrides.<chart>
--
-- Image references are prefixed with the local registry address
-- (10.0.2.2:5000 -- u7s's slirp4netns gateway to the host, where
-- `just registry-up` runs a registry container on port 5000). See
-- `just cluster-load-all`, which pushes built images there via
-- skopeo rather than importing directly into a specific cluster's
-- containerd -- this works with any Kubernetes distribution whose
-- nodes can reach the registry, not just u7s.

let registry = "10.0.2.2:5000/"

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
, neo4j =
  { image              = registry ++ "neo4j:5.26.2"
  , certIssuerUrl      = "https://cert-issuer.polar.svc.cluster.local:8443"
  , certIssuerAudience = "polar-cert-issuer-neo4j.local"
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
, gitlab =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer = { image = registry ++ "gitlab-observer:latest" }
  , consumer = { image = registry ++ "gitlab-consumer:latest" }
  }
, git =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = registry ++ "git-observer:latest" }
  , consumer  = { image = registry ++ "git-processor:latest" }
  , scheduler = { image = registry ++ "git-scheduler:latest" }
  }

, build-processor =
  { imagePullSecrets = [] : List { name : Optional Text }
  , processor = { image = registry ++ "build-processor:latest" }
  }
, resolver =
  { imagePullSecrets = [] : List { name : Optional Text }
  , resolver = { image = registry ++ "oci-resolver:latest" }
  }
, scheduler =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = registry ++ "scheduler-observer:latest" }
  , processor = { image = registry ++ "scheduler-processor:latest" }
  }
, openapi =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = registry ++ "openapi-observer:latest", openapiEndpoint = "http://localhost:3000/api-docs/openapi.json" }
  , processor = { image = registry ++ "openapi-processor:latest" }
  }
, certIssuer =
  { image            = registry ++ "cert-issuer:latest"
  , polarInitImage   = registry ++ "polar-init:latest"
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
    ]
  , serverLifetimeSecs = 43200
  -- The CA is created by `polar-admin-grant bootstrap-ca` with the cluster
  -- admin's own kubeconfig and mounted from a Secret. Callers verify against
  -- the polar-ca ConfigMap (cert_issuer_ca_configmap in target.nu); render.nu
  -- refuses to render if the two disagree.
  , caProvisioning = CaProvisioning.Provisioned
  }
}
