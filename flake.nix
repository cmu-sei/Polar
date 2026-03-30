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
    nix-container-lib.url                         = "github:daveman1010221/nix-container-lib";
    nix-container-lib.inputs.nixpkgs.follows      = "nixpkgs";
    nix-container-lib.inputs.flake-utils.follows  = "flake-utils";
    # pi-agent-rust.url                         = "github:daveman1010221/pi_agent_rust";
    # pi-agent-rust.inputs.nixpkgs.follows      = "nixpkgs";
    # pi-agent-rust.inputs.rust-overlay.follows = "rust-overlay";
  };
  outputs = { self, nixpkgs, crane, rust-overlay, flake-utils, advisory-db,
              myNeovimOverlay, staticanalysis, dotacat, nix-container-lib, ... }:
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
          inputs = { inherit staticanalysis dotacat myNeovimOverlay rust-overlay; };
          inherit configNixPath;
        };

        # ── Dev container ──────────────────────────────────────────────────────
        # Regenerate with: cd src/flake && just render-dev
        container = mkPolarContainer ./src/flake/container.nix;

        # ── CI container ──────────────────────────────────────────────────────
        # Regenerate with: cd src/flake && just render-ci
        ciContainer = mkPolarContainer ./src/flake/ci-container.nix;

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
          inherit pkgs lib crane rust-overlay nix-container-lib system;
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
          webObserverImage     = polarPkgs.webAgent.observerImage;
          webConsumerImage     = polarPkgs.webAgent.consumerImage;

          # ── Provenance agent ─────────────────────────────────────────────────
          provenanceLinkerImage   = polarPkgs.provenance.linkerImage;
          provenanceResolverImage = polarPkgs.provenance.resolverImage;

          # ── Scheduler agent ──────────────────────────────────────────────────
          schedulerProcessorImage = polarPkgs.scheduler.processorImage;
          schedulerObserverImage  = polarPkgs.scheduler.observerImage;

          # ── Jira agent ───────────────────────────────────────────────────────
          jiraObserverImage    = polarPkgs.jiraAgent.observerImage;
          jiraConsumerImage    = polarPkgs.jiraAgent.consumerImage;

          # ── Git agent ────────────────────────────────────────────────────────
          gitObserverImage     = polarPkgs.gitAgent.observerImage;
          gitConsumerImage     = polarPkgs.gitAgent.consumerImage;
          gitSchedulerImage    = polarPkgs.gitAgent.schedulerImage;

          # ── Build orchestrator ───────────────────────────────────────────────
          orchestratorImage    = polarPkgs.buildOrchestrator.orchestratorImage;
          buildProcessorImage  = polarPkgs.buildOrchestrator.buildProcessorImage;
        };

        devShells.default = container.devShell;
      });
}
