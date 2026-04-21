-- src/flake/agent-container.dhall
--
-- Polar agent container configuration.
-- An interactive AI coding agent runtime with:
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
-- To update the pinned prelude after a nix-container-lib release:
--   nix-prefetch-git https://github.com/daveman1010221/nix-container-lib
--   dhall hash <<< "https://raw.githubusercontent.com/daveman1010221/nix-container-lib/<rev>/dhall/prelude.dhall"

let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/9da4924831c8e0d81d57448425d6cd10820b71d2/dhall/prelude.dhall
        sha256:18acbbb5708565905ab9522fa77a81eb402851f06870a34a22f6c979001c4571

let defaults = Lib.defaults

-- ---------------------------------------------------------------------------
-- Agent-specific package layer
-- llama.cpp ROCm + supporting tools
-- ---------------------------------------------------------------------------
let agentTools =
  Lib.customLayer "polar-agent-tools"
    [ Lib.flakePackage "llamaCpp" "default"
    , Lib.nixpkgs "sqlite"
    , Lib.nixpkgs "curl"
    , Lib.flakePackage "dotacat" "default"
    , Lib.nixpkgs "sudo"
    , Lib.nixpkgs "just"
    , Lib.nixpkgs "moreutils"
    ]

-- ---------------------------------------------------------------------------
-- Supplemental groups for GPU access
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
  , Lib.runtimeEnv "ANTHROPIC_API_KEY"  ""
  , Lib.runtimeEnv "OPENAI_API_KEY"     ""
  , Lib.runtimeEnv "OPENROUTER_API_KEY" ""
  ]

-- ---------------------------------------------------------------------------
-- The agent container configuration.
-- Derived from defaults.devContainer (not agentContainer) because we want
-- the full interactive shell experience for debugging and direct use.
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

  , shell = Some (Lib.Shell.Interactive
      { shell = "/bin/fish"
      , colorScheme = "gruvbox"
      , viBindings = True
      , plugins = [ "bobthefish", "bass", "grc" ]
      })

  , tls = Some
      ( defaults.defaultTLS //
        { generateCerts = True }
      )

  , ssh = Some defaults.defaultSSH

  , user = defaults.defaultUser //
      { supplementalGroups = gpuGroups }

  , pipeline = None Lib.PipelineConfig

  , extraEnv = agentEnv

  , ai = Some
      ( defaults.defaultAi //
        { enable = True }
      )
  }
