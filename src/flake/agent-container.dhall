-- src/flake/agent-container.dhall
--
-- Polar AI agent container configuration.
-- A minimal nushell runtime with local LLM inference via llama.cpp (ROCm).
--
-- To update the pinned prelude after a nix-container-lib release:
--   just update-dhall-pins

let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/e2334448bd4bb6348a467244d474f907d3d0e36d/dhall/prelude.dhall
        sha256:b81e69ef2fe811bc853a8a9a0202c0af802f7cd53c78f95f67083bf3dceee86b

let defaults = Lib.defaults

let agentTools =
  Lib.customLayer "polar-agent-tools"
    [ Lib.flakePackage "llamaCpp" "default"
    , Lib.nixpkgs "just"
    , Lib.nixpkgs "curl"
    ]

let gpuGroups =
  [ { name = "video",  gid = 44  }
  , { name = "render", gid = 110 }
  ]

let agentEnv : List Lib.EnvVar =
  [ Lib.buildEnv "LLAMA_HOST"       "0.0.0.0"
  , Lib.buildEnv "LLAMA_PORT"       "8080"
  , Lib.buildEnv "LLAMA_CTX_SIZE"   "32768"
  , Lib.buildEnv "LLAMA_GPU_LAYERS" "99"
  , Lib.buildEnv "LLAMA_BASE_URL"   "http://localhost:8080/v1"
  , Lib.startEnv "OLLAMA_HOST"      "0.0.0.0:8080"
  , Lib.runtimeEnv "ANTHROPIC_API_KEY"  ""
  , Lib.runtimeEnv "OPENAI_API_KEY"     ""
  , Lib.runtimeEnv "OPENROUTER_API_KEY" ""
  ]

in defaults.aiAgentContainer //
  { name = "polar-agent"
  , packageLayers =
      [ Lib.PackageLayer.Micro
      , Lib.PackageLayer.Core
      , Lib.PackageLayer.RustToolchain
      , agentTools
      ]
  , user = defaults.defaultUser //
      { supplementalGroups = gpuGroups }
  , ai = Some
      ( defaults.defaultAi //
        { enable = True }
      )
  , extraEnv = agentEnv
  }
