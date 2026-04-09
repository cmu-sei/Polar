# src/agents/build-orchestrator/package.nix
#
# Packages the build-orchestrator binary and its container image, plus
# the cyclops git-clone init container. Previously the clone init container
# lived in a standalone flake.nix — it has been pulled into the main workspace
# so it shares polar's nixpkgs pin and uses nix-container-lib consistently.

{ pkgs
, commonPaths
, craneLib
, crateArgs
, workspaceFileset
, commonUser
, nix-container-lib   # passed from workspace.nix
, inputs
, system
}:

let

  # ---------------------------------------------------------------------------
  # Build orchestrator binary and image
  # Same pattern as cassini, gitlab, kube, etc.
  # ---------------------------------------------------------------------------
  orchestrator = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-orchestrator";
    src            = workspaceFileset ./agent;
    doCheck        = false;
  });

  orchestratorEnv = pkgs.buildEnv {
    name         = "orchestrator-env";
    paths        = commonPaths ++ [ orchestrator ];
    pathsToLink  = [ "/bin" "/etc/ssl/certs" ];
  };

  extraCommands = ''
    mkdir -p etc
    printf 'polar:x:1000:1000::/home/polar:/bin/bash\n' > etc/passwd
    printf 'polar:x:1000:\n' > etc/group
    printf 'polar:!x:::::::\n' > etc/shadow
  '';

  orchestratorImage = pkgs.dockerTools.buildLayeredImage {
    name      = "build-orchestrator";
    tag       = "latest";
    contents  = commonPaths ++ [ orchestratorEnv ];  # was contents
    inherit extraCommands;
    config = {
      User       = "${commonUser.uid}:${commonUser.gid}";
      Cmd        = [ "build-orchestrator" ];
      WorkingDir = "/";
      Env        = [];
    };
  };

  buildProcessor = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-processor";
    src            = workspaceFileset ./processor;
    doCheck        = false;
  });

  buildProcessorEnv = pkgs.buildEnv {
    name         = "processor-env";
    paths        = commonPaths ++ [ buildProcessor ];  # fix: was orchestrator
    pathsToLink  = [ "/bin" "/etc/ssl/certs" ];
  };

  buildProcessorImage = pkgs.dockerTools.buildLayeredImage {
    name      = "build-processor";
    tag       = "latest";
    contents  = commonPaths ++ [ buildProcessorEnv ];  # was contents
    inherit extraCommands;
    config = {
      User       = "${commonUser.uid}:${commonUser.gid}";
      Cmd        = [ "build-processor" ];  # fix: was build-orchestrator
      WorkingDir = "/";
      Env        = [];
    };
  };

  # ---------------------------------------------------------------------------
  # Cyclops git-clone init container
  #
  # A minimal single-binary container that clones a git repo at a specific
  # commit into /workspace, then exits 0. Used as a Kubernetes init container
  # in Cyclops build jobs.
  #
  # Contract (env vars injected by the Cyclops orchestrator at runtime):
  #   CYCLOPS_REPO_URL    — full HTTPS URL of the repository
  #   CYCLOPS_COMMIT_SHA  — full 40-character commit SHA
  #   CYCLOPS_WORKSPACE   — destination path (always /workspace)
  #   GIT_USERNAME        — sourced from k8s Secret
  #   GIT_PASSWORD        — sourced from k8s Secret
  #
  # Security notes:
  #   - Credentials are injected as env vars from a k8s Secret, never written
  #     to disk. The GIT_ASKPASS approach keeps them out of the remote URL,
  #     git reflog, and process list.
  #   - The askpass script lives in /tmp (tmpfs) and is removed after clone.
  #   - Runs as UID 65532 (non-root, conventional for k8s init containers).
  #   - .git directory is stripped from workspace after checkout to reduce
  #     attack surface and workspace size for the downstream pipeline.
  # ---------------------------------------------------------------------------
  gitCloneEntrypoint = pkgs.writeShellApplication {
    name = "git-clone-entrypoint";

    runtimeInputs = [ pkgs.git pkgs.coreutils ];

    text = ''
      set -euo pipefail

      # ── Validate required env vars ─────────────────────────────────────────
      required_vars=(
        CYCLOPS_REPO_URL
        CYCLOPS_COMMIT_SHA
        CYCLOPS_WORKSPACE
        GIT_USERNAME
        GIT_PASSWORD
      )
      for var in "''${required_vars[@]}"; do
        if [[ -z "''${!var:-}" ]]; then
          echo "[clone] ERROR: required environment variable $var is not set" >&2
          exit 1
        fi
      done

      # Validate commit SHA is exactly 40 hex characters.
      if ! [[ "$CYCLOPS_COMMIT_SHA" =~ ^[0-9a-f]{40}$ ]]; then
        echo "[clone] ERROR: CYCLOPS_COMMIT_SHA must be a full 40-character SHA, got: $CYCLOPS_COMMIT_SHA" >&2
        exit 1
      fi

      echo "[clone] repo:   $CYCLOPS_REPO_URL"
      echo "[clone] commit: $CYCLOPS_COMMIT_SHA"
      echo "[clone] dest:   $CYCLOPS_WORKSPACE"

      # ── Credential helper ──────────────────────────────────────────────────
      askpass_script=/tmp/git-askpass.sh
      cat > "$askpass_script" << 'ASKPASS'
      #!/bin/sh
      case "$1" in
        *Username*) echo "$GIT_USERNAME" ;;
        *Password*) echo "$GIT_PASSWORD" ;;
      esac
      ASKPASS
      chmod 700 "$askpass_script"

      export GIT_ASKPASS="$askpass_script"
      export GIT_CONFIG_NOSYSTEM=1
      export HOME=/tmp

      # ── Safe directory ─────────────────────────────────────────────────────
      git config --global --add safe.directory "$CYCLOPS_WORKSPACE"

      # ── Clone ──────────────────────────────────────────────────────────────
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

      # ── Credential cleanup ─────────────────────────────────────────────────
      rm -f "$askpass_script"
      unset GIT_ASKPASS GIT_USERNAME GIT_PASSWORD

      # Strip .git directory — pipeline runner has no business running git
      # commands and the .git directory contains the remote URL.
      rm -rf "$CYCLOPS_WORKSPACE/.git"

      echo "[clone] done — workspace ready at $CYCLOPS_WORKSPACE"
    '';
  };

  # Clone init container for Cyclops build jobs.
  # container.dhall declares the image metadata, mode, uid/gid, and runtime
  # deps (git, cacert). The entrypoint script is built locally and passed via
  # extraDerivations since it cannot be expressed as a dhall PackageRef.
  cloneContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ gitCloneEntrypoint ];
  };

  cloneImage = cloneContainer.image;

in
{
  inherit orchestrator orchestratorImage cloneImage gitCloneEntrypoint buildProcessor buildProcessorImage;
}
