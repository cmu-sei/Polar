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
    , Lib.nixpkgs "kubectl"
    -- The Justfile's recipes are bash scripts (#!/usr/bin/env bash): port-forwards-up, user-cert-install and others.
    , Lib.nixpkgs "bash"
    , Lib.flakePackage "polar-tools" "default"
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
