-- infra/targets/local-minimal/overrides.dhall
--
-- Minimal target value overrides, scoped per chart.
-- Covers: neo4j, cassini, cert-issuer, jaeger, jira, kube agents.
-- Each chart's main.nu merges: values.dhall // overrides.<chart>

{ namespaces  = {=}
, storage     = {=}
, certManager = {=}

, jaeger =
  { image       = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
  , serviceType = "NodePort"
  }

, neo4j =
  { image              = "nix-neo4j:latest"
  , certIssuerUrl      = "http://cert-issuer.polar.svc.cluster.local:8443"
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
  { image           = "cassini:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  }

, jira =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = "jira-observer:latest", jiraUrl = "https://daveman1010220.atlassian.net", jiraEmail = Some "daveman1010220@gmail.com" }
  , processor = { image = "jira-processor:latest" }
  }

, kube =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer = { image = "kube-observer:latest" }
  , consumer = { image = "kube-consumer:latest" }
  }

, certIssuer =
  { image            = "cert-issuer:latest"
  , certClientImage  = "polar-cert-client:latest"
  , polarInitImage   = "polar-init:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  , oidcIssuerUrl    = "https://kubernetes.default.svc.cluster.local"
  , oidcAudience     = ["polar-cert-issuer.local", "polar-cert-issuer-neo4j.local"]
  , oidcJwksUri      = Some "https://kubernetes.default.svc.cluster.local/openid/v1/jwks"
  , serverLifetimeSecs = 43200
  }
}
