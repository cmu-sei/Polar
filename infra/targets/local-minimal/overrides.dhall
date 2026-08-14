-- infra/targets/local-minimal/overrides.dhall
--
-- Minimal target value overrides, scoped per chart.
-- Covers: neo4j, cassini, cert-issuer, jaeger, jira, kube agents.
-- Each chart's main.nu merges: values.dhall // overrides.<chart>
--
-- Image references are prefixed with the local registry address
-- (10.0.2.2:5000 -- u7s's slirp4netns gateway to the host, where
-- `just registry-up` runs a registry container on port 5000). See
-- `just cluster-load-all`/`cluster-load-minimal`, which push built
-- images there via skopeo rather than importing directly into a
-- specific cluster's containerd -- this works with any Kubernetes
-- distribution whose nodes can reach the registry, not just u7s.

let registry = "10.0.2.2:5000/"

in
{ namespaces  = {=}
, storage     = {=}
, certManager = {=}
, jaeger =
  { image       = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
  , serviceType = "NodePort"
  }
, ace =
  { image            = registry ++ "ace-viz:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  }
, neo4j =
  { image              = registry ++ "neo4j:5.26.2"
  , certIssuerUrl      = "http://cert-issuer.polar.svc.cluster.local:8443"
  , certIssuerAudience = "polar-cert-issuer-neo4j.local"
  , certClientImage    = registry ++ "polar-cert-client:latest"
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
, jira =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = registry ++ "jira-observer:latest", jiraUrl = "https://daveman1010220.atlassian.net", jiraEmail = Some "daveman1010220@gmail.com" }
  , processor = { image = registry ++ "jira-processor:latest" }
  }
, kube =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer = { image = registry ++ "kube-observer:latest" }
  , consumer = { image = registry ++ "kube-consumer:latest" }
  }
, certIssuer =
  { image            = registry ++ "cert-issuer:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  , oidcIssuerUrl    = "https://kubernetes.default.svc.cluster.local"
  , oidcAudience     = ["polar-cert-issuer.local", "polar-cert-issuer-neo4j.local"]
  , oidcJwksUri      = Some "https://kubernetes.default.svc.cluster.local/openid/v1/jwks"
  , serverLifetimeSecs = 43200
  , identityLifetimeOverrides =
    [ { identity = "default.polar-graph.serviceaccount.cluster.local", lifetimeSecs = 31536000 } ]
  }
}
