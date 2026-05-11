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
    nix-container-lib.url = "github:daveman1010221/nix-container-lib";
    nix-container-lib.inputs.nixpkgs.follows      = "nixpkgs";
    nix-container-lib.inputs.flake-utils.follows  = "flake-utils";
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

        # ── Container lib helper ───────────────────────────────────────────────
        mkPolarContainer = configNixPath: nix-container-lib.lib.${system}.mkContainer {
          inherit system pkgs;
          inputs = {
            inherit staticanalysis dotacat myNeovimOverlay rust-overlay;
            cassini-client = {
              packages.${system}.default = polarPkgs.cassini.client;
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
        };

        # ── Agent containers ───────────────────────────────────────────────────
        # Regenerate with: cd src/flake && just render-agent
        mkAgentContainer = llamaCppPkg: extraPkgs:
          nix-container-lib.lib.${system}.mkContainer {
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
          inherit pkgs lib crane rust-overlay nix-container-lib inputs system;
        };

        containerPkgs = import ./src/containers/workspace.nix {
          inherit pkgs lib nix-container-lib inputs system;
        };

        commitMsgHooksPkg = import ./src/git-hooks/package.nix {
          inherit pkgs crane;
        };

        # ── TLS certificates ───────────────────────────────────────────────────
        tlsCerts = pkgs.callPackage ./src/flake/gen-certs.nix { inherit pkgs; };

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
          inherit polarPkgs tlsCerts;

          # ── Dev / CI / Agent containers ──────────────────────────────────────
          devContainer         = container.image;
          ciContainer          = ciContainer.image;
          agentContainer       = (mkAgentContainer pkgs.llama-cpp pkgs.stdenv.cc).image;
          agentContainerRocm   = (mkAgentContainer pkgs.llama-cpp-rocm pkgs.stdenv.cc).image;
          agentContainerVulkan = (mkAgentContainer pkgs.llama-cpp-vulkan pkgs.stdenv.cc).image;
          agentContainerNvidia = (mkAgentContainer pkgsCuda.llama-cpp pkgsCuda.cudaPackages.cuda_cudart).image;

          # ── Certificate Issuer ──────────────────────────────────────────────────────────
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

          # ── Provenance agent ─────────────────────────────────────────────────
          provenanceLinkerImage   = polarPkgs.provenance.linkerImage;
          provenanceResolverImage = polarPkgs.provenance.resolverImage;

          # ── Scheduler agent ──────────────────────────────────────────────────
          schedulerProcessorImage = polarPkgs.scheduler.processorImage;
          schedulerObserverImage  = polarPkgs.scheduler.observerImage;

          # ── Jira agent ───────────────────────────────────────────────────────
          jiraObserverImage    = polarPkgs.jiraAgent.observerImage;
          jiraProcessorImage   = polarPkgs.jiraAgent.processorImage;

          # ── Git agent ────────────────────────────────────────────────────────
          gitObserverImage     = polarPkgs.gitAgent.observerImage;
          gitProcessorImage    = polarPkgs.gitAgent.processorImage;
          gitSchedulerImage    = polarPkgs.gitAgent.schedulerImage;

          # ── Build orchestrator ───────────────────────────────────────────────
          orchestratorImage    = polarPkgs.buildOrchestrator.orchestratorImage;
          buildProcessorImage  = polarPkgs.buildOrchestrator.buildProcessorImage;
          cloneImage           = polarPkgs.buildOrchestrator.cloneImage;

          # ── Infrastructure containers ─────────────────────────────────────────
          nuInitImage = polarPkgs.nuInit.image;
          gitServerImage = polarPkgs.gitServer.image;
        };

        devShells.default = container.devShell.overrideAttrs (old: {
          shellHook = old.shellHook + ''
            export GRAPH_DB="neo4j"
            export GRAPH_ENDPOINT="bolt://127.0.0.1:7687"
            export GRAPH_USER="neo4j"

            export BIND_ADDR="127.0.0.1:8080"
            export CASSINI_BIND_ADDR="$BIND_ADDR"
            export BROKER_ADDR="$BIND_ADDR"
            export CONTROLLER_BIND_ADDR="127.0.0.1:3030"
            export CONTROLLER_ADDR="$CONTROLLER_BIND_ADDR"

            export CASSINI_SERVER_NAME="localhost"
            export CONTROLLER_SERVER_NAME="localhost"
            export HARNESS_SERVER_NAME="localhost"
            export CASSINI_SHUTDOWN_TOKEN="HEYTHERE"

            export RUST_LOG=trace
            export JAEGER_ENABLE_TRACING=1
            export JAEGER_OTLP_ENDPOINT="http://localhost:4318/v1/traces"

            export TMPDIR=$(mktemp -d)

            export POLAR_SCHEDULER_LOCAL_PATH="~/Documents/projects/polar-sched-test"
            export POLAR_SCHEDULER_REMOTE_URL="https://github.com/daveman1010221/polar-schedules.git"
            export POLAR_SCHEDULER_SYNC_INTERVAL="120"

            echo ""
            echo "Polar dev shell — cluster startup sequence:"
            echo "  cd ~/Documents/projects/nix-usernetes"
            echo "  just load-node-image && just reset && just up && just init && just kubeconfig"
            echo "  export KUBECONFIG=\$(pwd)/kubeconfig"
            echo "  cd ~/Documents/projects/Polar"
            echo "  just cluster-install-cert-manager"
            echo "  just cluster-render && just cluster-apply-storage && just cluster-load-all && just cluster-apply"
            echo ""
            export SOPS_AGE_BINARY=$(which rage)
            export SOPS_AGE_SSH_PRIVATE_KEY_FILE=~/.ssh/id_github
          '';
        });
      });
}
