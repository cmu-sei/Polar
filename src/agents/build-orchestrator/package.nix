# src/agents/build-orchestrator/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, healthcheckDrv
, ...
}:
let
  orchestrator = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin build-orchestrator";
    src            = workspaceFileset ./.;
    doCheck        = false;
  });

  # Graph-driven work queue for the orchestrator's analysis jobs.
  analysisScheduler = craneLib.buildPackage (crateArgs // {
    cargoExtraArgs = "--bin analysis-scheduler";
    src            = workspaceFileset ./.;
    doCheck        = false;
  });

  healthcheckInputs = inputs // {
    polar-healthcheck = {
      packages.${system}.default = healthcheckDrv;
    };
  };

  gitCloneEntrypoint = pkgs.writeShellApplication {
    name = "git-clone-entrypoint";
    runtimeInputs = [ pkgs.git pkgs.coreutils pkgs.curl pkgs.gnutar pkgs.gzip ];
    text = ''
      set -euo pipefail

      # CYCLOPS_SOURCE_KIND is set by the orchestrator (see SourceRef::fetch_env).
      # Unset means git, for orchestrators that predate archive sources.
      source_kind="''${CYCLOPS_SOURCE_KIND:-git}"

      if [[ "$source_kind" == "archive" ]]; then
        for var in CYCLOPS_ARCHIVE_URL CYCLOPS_ARCHIVE_SHA256 CYCLOPS_WORKSPACE; do
          if [[ -z "''${!var:-}" ]]; then
            echo "[fetch] ERROR: required environment variable $var is not set" >&2
            exit 1
          fi
        done

        if ! [[ "$CYCLOPS_ARCHIVE_SHA256" =~ ^[0-9a-f]{64}$ ]]; then
          echo "[fetch] ERROR: CYCLOPS_ARCHIVE_SHA256 must be a 64-character lowercase hex digest" >&2
          exit 1
        fi
        if [[ "$CYCLOPS_ARCHIVE_URL" != https://* ]]; then
          echo "[fetch] ERROR: CYCLOPS_ARCHIVE_URL must be https, got: $CYCLOPS_ARCHIVE_URL" >&2
          exit 1
        fi

        echo "[fetch] purl:    ''${CYCLOPS_SOURCE_PURL:-<none>}"
        echo "[fetch] archive: $CYCLOPS_ARCHIVE_URL"
        echo "[fetch] sha256:  $CYCLOPS_ARCHIVE_SHA256"
        echo "[fetch] dest:    $CYCLOPS_WORKSPACE"

        archive="$(mktemp /tmp/cyclops-archive.XXXXXX)"
        curl --fail --silent --show-error --location --proto '=https' \
          --output "$archive" "$CYCLOPS_ARCHIVE_URL"

        # Verify before extracting: nothing from an unverified archive ever
        # reaches the workspace.
        actual="$(sha256sum "$archive" | cut -d' ' -f1)"
        if [[ "$actual" != "$CYCLOPS_ARCHIVE_SHA256" ]]; then
          echo "[fetch] ERROR: sha256 mismatch: expected $CYCLOPS_ARCHIVE_SHA256, got $actual" >&2
          rm -f "$archive"
          exit 1
        fi
        echo "[fetch] sha256 verified"

        # Registry archives (crates.io, npm, PyPI sdists) wrap their contents
        # in one top-level directory; strip it so /workspace is the package root.
        mkdir -p "$CYCLOPS_WORKSPACE"
        tar --extract --gzip --file "$archive" \
          --directory "$CYCLOPS_WORKSPACE" \
          --strip-components=1 \
          --no-same-owner --no-same-permissions
        rm -f "$archive"

        echo "[fetch] done — workspace ready at $CYCLOPS_WORKSPACE"
        exit 0
      fi

      if [[ "$source_kind" != "git" ]]; then
        echo "[clone] ERROR: unknown CYCLOPS_SOURCE_KIND: $source_kind" >&2
        exit 1
      fi

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
    inherit system pkgs;
    inputs           = healthcheckInputs;
    configNixPath    = ./container-orchestrator.nix;
    extraDerivations = [ orchestrator ];
  };

  schedulerContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs           = healthcheckInputs;
    configNixPath    = ./container-scheduler.nix;
    extraDerivations = [ analysisScheduler ];
  };

  cloneContainer = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs inputs;
    configNixPath    = ./container.nix;
    extraDerivations = [ gitCloneEntrypoint ];
  };

in
{
  inherit orchestrator analysisScheduler gitCloneEntrypoint;
  orchestratorImage  = orchestratorContainer.image;
  schedulerImage     = schedulerContainer.image;
  cloneImage         = cloneContainer.image;
}
