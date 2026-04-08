-- src/deploy/local/values.local.dhall
-- Local kind overrides for values.dhall.
-- Import this instead of values.dhall when rendering manifests for kind.
-- Keeps sandbox-specific refs out of the base file.

let base = ./values.dhall

-- No image pull secrets needed — all images are kind-loaded locally.
let imagePullSecrets = [] : List { name : Optional Text }

-- Local image refs (nix build outputs, kind-loaded).
-- Tags are always "latest" since that is what the nix package.nix files set.
in base //
  { imagePullSecrets

  , cassini = base.cassini //
    { image = "cassini:latest" }

  , gitlabObserver = base.gitlabObserver //
    { image = "polar-gitlab-observer:latest" }

  , gitlabConsumer = base.gitlabConsumer //
    { image = "polar-gitlab-consumer:latest" }

  , kubeObserver = base.kubeObserver //
    { image = "polar-kube-observer:latest" }

  , kubeConsumer = base.kubeConsumer //
    { image = "polar-kube-consumer:latest" }

  , gitObserver = base.gitObserver //
    { image = "polar-git-repo-observer:latest" }

  , gitConsumer = base.gitConsumer //
    { image = "polar-git-consumer:latest" }

  , gitScheduler = base.gitScheduler //
    { image = "polar-git-scheduler:latest" }

  , linker = base.linker //
    { image = "provenance-linker-agent:latest" }

  , resolver = base.resolver //
    { image = "provenance-resolver-agent:latest" }

  , neo4j = base.neo4j //
    { image = "nix-neo4j:latest" }

  , buildOrchestrator = base.buildOrchestrator //
    { image = "build-orchestrator:latest" }

  , buildProcessor = base.buildProcessor //
    { image = "build-processor:latest" }

  , isLocal = True
  }
