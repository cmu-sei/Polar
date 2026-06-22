# src/agents/build-orchestrator/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, ...
}:
let
  orchestrator = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-orchestrator";
    src            = workspaceFileset ./build-orchestrator/agent;
    doCheck        = false;
  });

  buildProcessor = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-processor";
    src            = workspaceFileset ./build-orchestrator/processor;
    doCheck        = false;
  });

  gitCloneEntrypoint = pkgs.writeShellApplication {
    name = "git-clone-entrypoint";
    runtimeInputs = [ pkgs.git pkgs.coreutils ];
    text = ''
      set -euo pipefail

      required_vars=(
        CYCLOPS_REPO_URL
        CYCLOPS_COMMIT_SHA
        CYCLOPS_WORKSPACE
      )
      for var in "''${required_vars[@]}"; do
        if [[ -z "''${!var:-}" ]]; then
          echo "[clone] ERROR: required environment variable $var is not set" >&2
          exit 1
        fi
      done

      if ! [[ "$CYCLOPS_COMMIT_SHA" =~ ^[0-9a-f]{40}$ ]]; then
        echo "[clone] ERROR: CYCLOPS_COMMIT_SHA must be a full 40-character SHA, got: $CYCLOPS_COMMIT_SHA" >&2
        exit 1
      fi

      echo "[clone] repo:   $CYCLOPS_REPO_URL"
      echo "[clone] commit: $CYCLOPS_COMMIT_SHA"
      echo "[clone] dest:   $CYCLOPS_WORKSPACE"

      export GIT_CONFIG_NOSYSTEM=1
      export HOME=/tmp

      git config --global --add safe.directory "$CYCLOPS_WORKSPACE"

      mkdir -p "$CYCLOPS_WORKSPACE"

      echo "[clone] cloning (partial)..."
      git clone \
        --no-checkout \
        --filter=blob:none \
        --depth=1 \
        "$CYCLOPS_REPO_URL" \
        "$CYCLOPS_WORKSPACE"

      cd "$CYCLOPS_WORKSPACE"

      echo "[clone] fetching commit $CYCLOPS_COMMIT_SHA..."
      git fetch \
        --depth=1 \
        origin \
        "$CYCLOPS_COMMIT_SHA" \
        || {
          echo "[clone] direct SHA fetch unsupported — fetching full default branch..."
          git fetch --unshallow origin
        }

      echo "[clone] checking out $CYCLOPS_COMMIT_SHA..."
      git checkout "$CYCLOPS_COMMIT_SHA"

      rm -rf "$CYCLOPS_WORKSPACE/.git"

      echo "[clone] done — workspace ready at $CYCLOPS_WORKSPACE"
    '';
  };

  orchestratorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-orchestrator.nix;
    extraDerivations = [ orchestrator ];
  };

  processorContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container-processor.nix;
    extraDerivations = [ buildProcessor ];
  };

  cloneContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ gitCloneEntrypoint ];
  };

in
{
  inherit orchestrator buildProcessor gitCloneEntrypoint;
  orchestratorImage  = orchestratorContainer.image;
  buildProcessorImage = processorContainer.image;
  cloneImage         = cloneContainer.image;
}
