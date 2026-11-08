# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

{
  description =
  "
  This flake represents the build configurations for the Polar project.
  It outputs binaries and container images for each service,
  as well as containerized development and test environments for ease of use.
  See documentation for additional details.
  ";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
    myNeovimOverlay.url                           = "github:daveman1010221/nix-neovim";
    myNeovimOverlay.inputs.nixpkgs.follows        = "nixpkgs";
    myNeovimOverlay.inputs.flake-utils.follows    = "flake-utils";
    staticanalysis.url                            = "github:daveman1010221/polar-static-analysis";
    staticanalysis.inputs.nixpkgs.follows         = "nixpkgs";
    staticanalysis.inputs.flake-utils.follows     = "flake-utils";
    staticanalysis.inputs.rust-overlay.follows    = "rust-overlay";
    dotacat.url                                   = "github:daveman1010221/dotacat-fast";
    dotacat.inputs.nixpkgs.follows                = "nixpkgs";
    # On feat/rootless-session until it merges: `imageName` (each image is `localhost/<name>`) and the rootless session.
    nix-container-lib.url = "github:daveman1010221/nix-container-lib/feat/rootless-session";
    nix-container-lib.inputs.nixpkgs.follows      = "nixpkgs";
    nix-container-lib.inputs.flake-utils.follows  = "flake-utils";
    # Scudo, every binary's allocator (docs/architecture/broker-baseline.md, "Decided"): LLVM's source with a flake that
    # builds it with our configuration.
    nix-scudo.url                                 = "github:daveman1010221/nix-scudo";
    nix-scudo.inputs.nixpkgs.follows              = "nixpkgs";
    nix-scudo.inputs.flake-utils.follows          = "flake-utils";
  };
  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils, advisory-db,
            myNeovimOverlay, staticanalysis, dotacat, nix-container-lib, ... }@inputs:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config = {
            cc = "clang";
            allowUnfree = true;
          };
          documentation = {
            dev.enable = true;
            man = {
              man-db.enable  = true;
              generateCaches = true;
            };
          };
          overlays = [
            rust-overlay.overlays.default
            myNeovimOverlay.overlays.default
          ];
        };
        inherit (pkgs) lib;

        # ── Dhall cache ────────────────────────────────────────────────────────
        # The pinned remote imports of the repository's Dhall, so a render fetches nothing (src/flake/dhall-cache.nix).
        dhallCache = import ./src/flake/dhall-cache.nix { inherit pkgs lib; roots = [ ./infra ./src ]; };

        # ── The platform version ───────────────────────────────────────────────
        # The repository's VERSION, the one place it is written: X.Y.Z at a release, X.Y.Z+dev between them
        # (infra/INFRA.md, "The release"). The Rust builds embed it (src/agents/polar-version), and every image made with
        # the container lib carries it as the label org.opencontainers.image.version. Only the file's contents are used,
        # so a commit that leaves it alone changes no binary and no image. The commit is never a label: it would change
        # every image's digest at every commit, a documentation commit included, and a release's lock could not be
        # reproduced from its tag. It is in the release's lock and changelog instead.
        polarVersion = lib.fileContents ./VERSION;
        versionLabels = { "org.opencontainers.image.version" = polarVersion; };

        # The container lib, with the version label on every image it makes (its extraConfig is merged over its own
        # Labels, so the title and the rest stay), and every image named `localhost/<config name>`: an image built here
        # and loaded by hand is in a node's own store, and says so the way podman does. Docker reads a hostless name as
        # Docker Hub's, and the OCI resolver does too. Registry paths (images.nuon) are unaffected: images.nu pushes to
        # `<registry>/<image>` whatever the archive calls it.
        containerLib = nix-container-lib // {
          lib = nix-container-lib.lib // {
            ${system} = nix-container-lib.lib.${system} // {
              mkContainer = args: nix-container-lib.lib.${system}.mkContainer (args // {
                extraConfig = lib.recursiveUpdate { Labels = versionLabels; } (args.extraConfig or { });
                imageName = args.imageName or "localhost/${(import args.configNixPath).name}";
              });
            };
          };
        };

        # ── Container lib helper ───────────────────────────────────────────────
        mkPolarContainer = configNixPath: containerLib.lib.${system}.mkContainer {
          inherit system pkgs;
          inputs = {
            inherit staticanalysis dotacat myNeovimOverlay rust-overlay;
            cassini-client = {
              packages.${system}.default = polarPkgs.cassini.client;
            };
            polar-healthcheck = {
              packages.${system}.default = polarPkgs.healthcheck;
            };
            polar-tools = {
              packages.${system}.default = polarPkgs.polarTools.tools;
            };
            polar-dhall-cache = {
              packages.${system}.default = dhallCache;
            };
            polar-scudo = {
              packages.${system}.default = polarPkgs.scudo;
            };
          };
          inherit configNixPath;
        };

        # ── Dev container ──────────────────────────────────────────────────────
        # Regenerate with: cd src/flake && just render-dev
        container = mkPolarContainer ./src/flake/container.nix;

        # ── CI container ──────────────────────────────────────────────────────
        ciContainer = import ./src/flake/ci-container.nix {
          inherit pkgs lib system;
          cassiniClient = polarPkgs.cassini.client;
          certIssuerClient = polarPkgs.certIssuer.client;
          scudo = polarPkgs.scudo;
        };

        # ── Agent containers ───────────────────────────────────────────────────
        # Regenerate with: cd src/flake && just render-agent
        mkAgentContainer = llamaCppPkg: extraPkgs:
          containerLib.lib.${system}.mkContainer {
            inherit system pkgs;
            inputs = {
              inherit staticanalysis dotacat myNeovimOverlay rust-overlay;
              llamaCpp = { packages.${system} = { default = llamaCppPkg; }; };
              cudaLibs = { packages.${system} = { default = extraPkgs; }; };
            };
            configNixPath = ./src/flake/agent-container.nix;
          };


        # ── Workspace binaries + agent images ─────────────────────────────────
        polarPkgs = import ./src/agents/workspace.nix {
          inherit pkgs lib crane rust-overlay inputs system polarVersion;
          nix-container-lib = containerLib;
        };

        containerPkgs = import ./src/containers/workspace.nix {
          inherit pkgs lib inputs system;
          nix-container-lib = containerLib;
        };

        commitMsgHooksPkg = import ./src/git-hooks/package.nix {
          inherit pkgs crane;
        };

        pkgsCuda = import nixpkgs {
          inherit system;
          config = {
            cc = "clang";
            allowUnfree = true;
            cudaSupport = true;
          };
          overlays = [
            rust-overlay.overlays.default
            myNeovimOverlay.overlays.default
          ];
        };

      in
      {
        packages = {
          # ── Workspace binaries ───────────────────────────────────────────────
          default     = polarPkgs.workspacePackages;
          inherit polarPkgs;

          inherit dhallCache;

          # ── Dev / CI / Agent containers ──────────────────────────────────────
          devContainer         = container.image;
          ciContainer          = ciContainer.image;
          agentContainer       = (mkAgentContainer pkgs.llama-cpp pkgs.stdenv.cc).image;
          agentContainerRocm   = (mkAgentContainer pkgs.llama-cpp-rocm pkgs.stdenv.cc).image;
          agentContainerVulkan = (mkAgentContainer pkgs.llama-cpp-vulkan pkgs.stdenv.cc).image;
          agentContainerNvidia = (mkAgentContainer pkgsCuda.llama-cpp pkgsCuda.cudaPackages.cuda_cudart).image;

          # ── Certificate Issuer ───────────────────────────────────────────────
          certIssuerImage         = polarPkgs.certIssuer.serverImage;

          # ── Cassini ──────────────────────────────────────────────────────────
          cassiniImage         = polarPkgs.cassini.cassiniImage;
          cassiniProducerImage = polarPkgs.cassini.producerImage;
          cassiniSinkImage     = polarPkgs.cassini.sinkImage;

          # ── GitLab agent ─────────────────────────────────────────────────────
          gitlabObserverImage  = polarPkgs.gitlabAgent.observerImage;
          gitlabConsumerImage  = polarPkgs.gitlabAgent.consumerImage;

          # ── Kubernetes agent ─────────────────────────────────────────────────
          kubeObserverImage    = polarPkgs.kubeAgent.observerImage;
          kubeConsumerImage    = polarPkgs.kubeAgent.consumerImage;

          # ── Web (OpenAPI) agent ───────────────────────────────────────────────
          openApiObserverImage  = polarPkgs.webAgent.observerImage;
          openApiProcessorImage = polarPkgs.webAgent.processorImage;

          ociResolverImage = polarPkgs.ociResolver.resolverImage;

          # ── Scheduler agent ──────────────────────────────────────────────────
          schedulerProcessorImage = polarPkgs.scheduler.processorImage;
          schedulerObserverImage  = polarPkgs.scheduler.observerImage;

          # ── Jira agent ───────────────────────────────────────────────────────
          jiraObserverImage    = polarPkgs.jiraAgent.observerImage;
          jiraProcessorImage   = polarPkgs.jiraAgent.processorImage;

          # ── Git agent ────────────────────────────────────────────────────────
          gitObserverImage     = polarPkgs.gitAgent.observerImage;
          gitProcessorImage    = polarPkgs.gitAgent.processorImage;
          gitControllerImage   = polarPkgs.gitAgent.controllerImage;

          # ── The state agent ──────────────────────────────────────────────────
          softStateObserverImage = polarPkgs.softState.observerImage;
          softStateProcessorImage = polarPkgs.softState.processorImage;

          # ── Credentials agent ────────────────────────────────────────────────
          credentialsAgentConsumerImage = polarPkgs.credentialsAgent.consumerImage;
          credentialsAgentObserverImage = polarPkgs.credentialsAgent.observerImage;

          buildProcessorImage  = polarPkgs.buildProcessor.buildProcessorImage;

          # ── Infrastructure containers ─────────────────────────────────────────
          nuInitImage = polarPkgs.nuInit.image;
          gitServerImage = polarPkgs.gitServer.image;
          polarGitServerImage = polarPkgs.polarGitServer.image;
          secretHarnessOwnerImage = polarPkgs.secretHarness.ownerImage;
          secretHarnessIntruderImage = polarPkgs.secretHarness.intruderImage;
          polarInitImage = polarPkgs.polarInit.image;
          polarOperatorImage = polarPkgs.polarOperator.image;
          buildOrchestratorImage = polarPkgs.buildOrchestrator.image;
          buildOrchestratorObjectStoreImage = polarPkgs.buildOrchestrator.objectStoreImage;
          polarCustodyImage = polarPkgs.polarCustody.image;
        };

        devShells.default = container.devShell.overrideAttrs (old: {
          shellHook = old.shellHook + ''
            export BIND_ADDR="127.0.0.1:8080"
            export CASSINI_BIND_ADDR="$BIND_ADDR"
            export BROKER_ADDR="$BIND_ADDR"
            export CONTROLLER_BIND_ADDR="127.0.0.1:3030"
            export CONTROLLER_ADDR="$CONTROLLER_BIND_ADDR"

            export CASSINI_SERVER_NAME="localhost"
            export CONTROLLER_SERVER_NAME="localhost"
            export HARNESS_SERVER_NAME="localhost"
            export CASSINI_SHUTDOWN_TOKEN="HEYTHERE"

            export RUST_LOG=info
            export JAEGER_ENABLE_TRACING=1
            export JAEGER_OTLP_ENDPOINT="http://localhost:4318/v1/traces"

            export TMPDIR=$(mktemp -d)

            export POLAR_SCHEDULER_LOCAL_PATH="$HOME/Documents/projects/polar-sched-test"
            export POLAR_SCHEDULER_REMOTE_URL="https://github.com/daveman1010221/polar-schedules.git"
            export POLAR_SCHEDULER_SYNC_INTERVAL="120"

            echo ""
            echo "Polar dev shell — startup sequence (steps marked [dev] are for nix-usernetes users):"
            echo "  cd ~/Documents/projects/Polar"
            echo "  nu infra/up.nu local-minimal --dev --user <user> --email <email>"
            echo "  # applies the target, waits, starts forwards, runs the admin flow, and pauses for"
            echo "  # the browser import and the Neo4j password; re-run the same command to resume"
            echo ""
            export POLAR_DHALL_CACHE="${dhallCache}/lib/polar/dhall-cache"
            export POLAR_SCUDO_LIB_DIR="${polarPkgs.scudo}/lib/polar/scudo"
            export SOPS_AGE_BINARY=$(which rage)
            export SOPS_AGE_SSH_PRIVATE_KEY_FILE=~/.ssh/id_github
          '';
        });
      });
}
