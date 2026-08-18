-- src/flake/container.dhall
--
-- Polar dev container configuration.
-- This file is the single place where polar-specific container decisions live.
-- All the how is in nix-container-lib. This file is pure what.
--
-- Library reference: github:daveman1010221/nix-container-lib
--

let Lib = ../containers/container-lib.dhall

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
    , Lib.nixpkgs "skopeo"
    , Lib.flakePackage "cassini-client" "default"
    , Lib.nixpkgs "git"
    , Lib.nixpkgs "curl"
    , Lib.nixpkgs "dhall"
    , Lib.nixpkgs "dhall-json"
    , Lib.nixpkgs "dhall-nix"
    , Lib.nixpkgs "dropbear"
    , Lib.nixpkgs "mermaid-cli"
    ]

let pythonDev =
  Lib.customLayer "python-dev"
    [ Lib.flakePackage "ace-python-env" "default"
    ]

-- ---------------------------------------------------------------------------
-- Project-specific environment variables.
-- BuildTime: arch-independent, no store paths — safe for config.Env.
-- ---------------------------------------------------------------------------
let polarEnv : List Lib.EnvVar =
  [ Lib.startEnv "GRAPH_DB"           "neo4j"
  , Lib.startEnv "GRAPH_ENDPOINT"     "bolt+s://localhost:7687"
  , Lib.startEnv "GRAPH_USER"         "neo4j"
  , Lib.startEnv "POLAR_HEALTH_FILE"  "/tmp/polar-health/polar-health.json"
  ]

-- ---------------------------------------------------------------------------
-- The container configuration.
-- Derived from defaults.devContainer with polar-specific overrides.
-- ---------------------------------------------------------------------------
in defaults.devContainer //
  { name = "polar-dev"

  , packageLayers =
      [ pythonDev
      , Lib.PackageLayer.Micro
      , Lib.PackageLayer.Core
      , Lib.PackageLayer.InteractiveDev
      , Lib.PackageLayer.RustToolchain
      , polarExtras
      ]

  , tls = Some
      ( defaults.defaultTLS //
        { generateCerts = True }
      )

  , ssh = Some
      ( defaults.defaultSSH //
        { enable = True
        , port   = 2223
        }
      )

  , extraEnv = polarEnv
  }
