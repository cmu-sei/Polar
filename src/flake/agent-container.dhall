-- src/flake/agent-container.dhall
--
-- Polar agent container configuration.
-- An interactive AI coding agent runtime with:
--   - pi_agent_rust: the coding agent binary
--   - llama.cpp (ROCm): local LLM inference with AMD GPU acceleration
--   - fish + bash shells available for debugging and interactive use
--   - GPU device group membership (video, render) for ROCm access
--   - TLS for secure agent communication
--
-- Usage:
--   podman run --rm -it \
--     --device /dev/kfd \
--     --device /dev/dri/renderD128 \
--     --group-add video \
--     --group-add render \
--     -e ANTHROPIC_API_KEY=sk-ant-... \
--     -v $PWD:/workspace \
--     localhost/polar-agent:latest
--
-- Or with local LLM:
--   start-llama /workspace/models/mymodel.gguf   # in the container
--   pi-local "Explain this codebase"              # in the container

let Lib      = PRELUDE_PATH
let defaults = Lib.defaults

-- ---------------------------------------------------------------------------
-- Agent-specific package layer
-- pi_agent_rust + llama.cpp ROCm + supporting tools
-- ---------------------------------------------------------------------------
let agentTools =
  Lib.customLayer "polar-agent-tools"
    [ -- pi_agent_rust is built as a local derivation in polar's flake
      -- and injected via the flake's packages, not via a flake input.
      -- It lands in the container via the devEnv buildEnv.
      -- llama.cpp with ROCm GPU support for AMD GPUs
      Lib.flakePackage "llamaCpp" "default"
      -- sqlite3 for pi's session storage
    , Lib.nixpkgs "sqlite"
      -- curl for pi-local health check and general HTTP work
    , Lib.nixpkgs "curl"
    , Lib.flakePackage "dotacat" "default"
    , Lib.flakePackage "piAgent" "default"
    , Lib.nixpkgs "sudo"
    , Lib.nixpkgs "just"
    ]

-- ---------------------------------------------------------------------------
-- Supplemental groups for GPU access
-- The container user needs to be in the video and render groups to access
-- /dev/kfd and /dev/dri/renderD128 for ROCm GPU acceleration.
-- GIDs 44 (video) and 110 (render) are conventional on Linux systems.
-- ---------------------------------------------------------------------------
let gpuGroups =
  [ { name = "video",  gid = 44  }
  , { name = "render", gid = 110 }
  ]

-- ---------------------------------------------------------------------------
-- Agent environment variables
-- API keys are UserProvided — injected at container run time, never baked in.
-- ---------------------------------------------------------------------------
let agentEnv : List Lib.EnvVar =
  [ Lib.buildEnv "LLAMA_HOST"       "0.0.0.0"
  , Lib.buildEnv "LLAMA_PORT"       "8080"
  , Lib.buildEnv "LLAMA_CTX_SIZE"   "32768"
  , Lib.buildEnv "LLAMA_GPU_LAYERS" "99"
  , Lib.buildEnv "LLAMA_BASE_URL"   "http://localhost:8080/v1"
  , Lib.buildEnv "PI_SHELL_PATH"    "/bin/bash"
  , Lib.startEnv "OLLAMA_HOST"      "0.0.0.0:8080"
    -- API keys — set at runtime, not baked in
  , Lib.runtimeEnv "ANTHROPIC_API_KEY"  ""
  , Lib.runtimeEnv "OPENAI_API_KEY"     ""
  , Lib.runtimeEnv "OPENROUTER_API_KEY" ""
  ]

-- ---------------------------------------------------------------------------
-- The agent container configuration
-- Derived from defaults.devContainer (not agentContainer) because we want
-- the full interactive shell experience for debugging and direct use.
-- The distinction from the dev container is: agent tools, GPU groups,
-- bash present alongside fish, and agent-specific env vars.
-- ---------------------------------------------------------------------------
in defaults.devContainer //
  { name = "polar-agent"

  , packageLayers =
      [ Lib.PackageLayer.Core
      , Lib.PackageLayer.CI
      , Lib.PackageLayer.Dev
      , Lib.PackageLayer.Toolchain
      , agentTools
      ]

  -- Shell: fish as default (because we prefer it) with bash available.
  -- pi uses bash internally for its bash tool regardless of the login shell.
  , shell = Some
      ( defaults.defaultShell //
        { shell = "/bin/fish" }
      )

  -- TLS: enabled for secure agent-to-service communication
  , tls = Some
      ( defaults.defaultTLS //
        { generateCerts = True }
      )

  -- SSH: present but not auto-started
  , ssh = Some defaults.defaultSSH

  -- User: add GPU groups for ROCm access
  , user = defaults.defaultUser //
      { supplementalGroups = gpuGroups }

  -- No pipeline for the agent container — it's a runtime, not a CI runner
  , pipeline = None Lib.PipelineConfig

  , extraEnv = agentEnv
    , ai = Some
        ( defaults.defaultAi //
          { enable = True }
        )
  }
