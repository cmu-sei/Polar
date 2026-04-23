-- infra/targets/local/overrides.dhall
--
-- Local target value overrides, scoped per chart.
-- Each chart's main.nu merges: values.dhall // overrides.<chart>

{ namespaces  = {=}
, storage     = {=}
, certManager = {=}

, jaeger =
  { image       = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
  , serviceType = "NodePort"
  }

, neo4j =
  { image          = "nix-neo4j:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  }

, cassini =
  { image           = "cassini:latest"
  , imagePullSecrets = [] : List { name : Optional Text }
  }

, jira =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = "jira-observer:latest" }
  , processor = { image = "jira-processor:latest" }
  }

, gitlab =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer = { image = "gitlab-observer:latest" }
  , consumer = { image = "gitlab-consumer:latest" }
  }

, kube =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer = { image = "kube-observer:latest" }
  , consumer = { image = "kube-consumer:latest" }
  }

, git =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = "git-observer:latest" }
  , consumer  = { image = "git-processor:latest" }
  , scheduler = { image = "git-scheduler:latest" }
  }

, provenance =
  { imagePullSecrets = [] : List { name : Optional Text }
  , linker   = { image = "provenance-linker-agent:latest" }
  , resolver = { image = "provenance-resolver-agent:latest" }
  }

, build =
  { imagePullSecrets = [] : List { name : Optional Text }
  , orchestrator = { image = "build-orchestrator:latest" }
  , processor    = { image = "build-processor:latest" }
  }

, scheduler =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = "polar-scheduler-observer:latest" }
  , processor = { image = "polar-scheduler-processor:latest" }
  }

, openapi =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = "openapi-observer:latest", openapiEndpoint = "http://localhost:3000/api-docs/openapi.json" }
  , processor = { image = "openapi-processor:latest" }
  }
}
