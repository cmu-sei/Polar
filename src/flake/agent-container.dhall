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

-- src/flake/agent-container.dhall
--
-- Polar AI agent container configuration.
-- A minimal nushell runtime with local LLM inference via llama.cpp (ROCm).
--

let Lib = ../containers/container-lib.dhall

let defaults = Lib.defaults

let agentTools =
  Lib.customLayer "polar-agent-tools"
    [ Lib.flakePackage "llamaCpp" "default"
    , Lib.nixpkgs "just"
    , Lib.nixpkgs "curl"
    , Lib.nixpkgs "rsync"
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
  , shell = Some (Lib.Shell.Interactive
      { shell       = "/bin/nu"
      , colorScheme = "gruvbox"
      , viBindings  = True
      , plugins     = [] : List Text
      })
  , packageLayers =
      [ Lib.PackageLayer.Micro
      , Lib.PackageLayer.Core
      , Lib.PackageLayer.RustToolchain
      , agentTools
      ]
  , pipeline = None Lib.PipelineConfig
  , tls = None Lib.TLSConfig
  , extraEnv = agentEnv
  , user = defaults.defaultUser //
      { supplementalGroups = gpuGroups }
  , ai = Some
      ( defaults.defaultAi //
        { enable = True }
      )
  }
