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
  , observer = { image = "polar-jira-observer:latest" }
  , consumer = { image = "polar-jira-consumer:latest" }
  }

, gitlab =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer = { image = "polar-gitlab-observer:latest" }
  , consumer = { image = "polar-gitlab-consumer:latest" }
  }

, kube =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer = { image = "polar-kube-observer:latest" }
  , consumer = { image = "polar-kube-consumer:latest" }
  }

, git =
  { imagePullSecrets = [] : List { name : Optional Text }
  , observer  = { image = "polar-git-repo-observer:latest" }
  , consumer  = { image = "polar-git-consumer:latest" }
  , scheduler = { image = "polar-git-scheduler:latest" }
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
}
