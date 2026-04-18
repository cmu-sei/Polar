-- src/flake/container.dhall
--
-- Polar dev container configuration.
-- This file is the single place where polar-specific container decisions live.
-- All the how is in nix-container-lib. This file is pure what.
--
-- Library reference: github:daveman1010221/nix-container-lib
--
-- To update the pinned prelude after a nix-container-lib release:
--   nix-prefetch-git https://github.com/daveman1010221/nix-container-lib
--   dhall hash <<< "https://raw.githubusercontent.com/daveman1010221/nix-container-lib/<rev>/dhall/prelude.dhall"

let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/bc1246f3372fbb825de2a85e6f3ca9d0779975d5/dhall/prelude.dhall
        sha256:42b061b5cb6c7685afaf7e5bc6210640d2c245e67400b22c51e6bfdf85a89e06

let defaults = Lib.defaults

-- ---------------------------------------------------------------------------
-- Polar-specific packages not covered by standard layers.
-- flakeInput names must match input names in flake.nix exactly.
-- ---------------------------------------------------------------------------
let polarExtras =
  Lib.customLayer "polar-extras"
    [ Lib.flakePackage "staticanalysis" "default"
    , Lib.flakePackage "dotacat"        "default"
    , Lib.flakePackage "myNeovimOverlay" "default"
    , Lib.nixpkgs "sops"
    , Lib.nixpkgs "oras"
    , Lib.nixpkgs "zed-editor"
    , Lib.nixpkgs "rage"
    , Lib.nixpkgs "cosign"
    , Lib.flakePackage "cassini-client" "default"
    ]

-- ---------------------------------------------------------------------------
-- Project-specific environment variables.
-- BuildTime: arch-independent, no store paths — safe for config.Env.
-- ---------------------------------------------------------------------------
let polarEnv : List Lib.EnvVar =
  [ Lib.buildEnv "GRAPH_DB"       "neo4j"
  , Lib.buildEnv "GRAPH_ENDPOINT" "bolt://127.0.0.1:7687"
  , Lib.buildEnv "GRAPH_USER"     "neo4j"
  ]

-- ---------------------------------------------------------------------------
-- The container configuration.
-- Derived from defaults.devContainer with polar-specific overrides.
-- ---------------------------------------------------------------------------
in defaults.devContainer //
  { name = "polar-dev"

  , packageLayers =
      [ Lib.PackageLayer.Core
      , Lib.PackageLayer.Dev
      , polarExtras
      ]

  , tls = Some
      ( defaults.defaultTLS //
        { generateCerts = True }
      )

  , ssh = Some
      ( defaults.defaultSSH //
        { enable = False
        , port   = 2223
        }
      )

  , extraEnv = polarEnv
  }
