{
  description = "Minimal git clone init container for Cyclops build jobs";

  inputs = {
    nixpkgs.url     = "github:NixOS/nixpkgs/nixos-24.11";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config.allowUnfree = false;
        };

        # ── Entrypoint ─────────────────────────────────────────────────────────
        #
        # The entrypoint script is the entire logic of this container.
        # It does exactly one thing: clone a git repo at a specific commit
        # into /workspace, then exit 0.
        #
        # Contract (env vars injected by the Cyclops orchestrator):
        #   CYCLOPS_REPO_URL    — full HTTPS URL of the repository
        #   BUILD_COMMIT_SHA  — full 40-character commit SHA
        #   BUILD_WORKSPACE   — destination path (always /workspace)
        #   GIT_USERNAME        — sourced from k8s Secret at runtime
        #   GIT_PASSWORD        — sourced from k8s Secret at runtime
        #
        # Security notes:
        # - Credentials are injected as env vars from a k8s Secret. They are
        #   never written to disk — git is configured with a transient
        #   credential helper that reads from the environment.
        # - The credential helper is unset immediately after the clone so it
        #   cannot be inherited by any child process.
        # - git config is written to a tmpfs path, not the image filesystem.
        # - No .git/config is persisted — the clone is shallow (depth 1 to
        #   the merge base) then the specific commit is checked out. This
        #   minimises data transferred and avoids pulling full history.
        # - The GIT_ASKPASS approach is used rather than embedding credentials
        #   in the remote URL, which would expose them in git's reflog and
        #   process list.
        entrypoint = pkgs.writeShellApplication {
          name = "git-clone-entrypoint";

          # git is the only runtime dependency. coreutils for mkdir/chmod.
          # No curl, no ssh, no package manager.
          runtimeInputs = [ pkgs.git pkgs.coreutils ];

          text = ''
            set -euo pipefail

            # ── Validate required env vars ─────────────────────────────────────
            required_vars=(
              BUILD_REPO_URL
              BUILD_COMMIT_SHA
              BUILD_WORKSPACE
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
            # A short SHA or branch name is a programming error — the orchestrator
            # must always resolve to a full SHA before submitting the job.
            if ! [[ "$BUILD_COMMIT_SHA" =~ ^[0-9a-f]{40}$ ]]; then
              echo "[clone] ERROR: BUILD_COMMIT_SHA must be a full 40-character SHA, got: $BUILD_COMMIT_SHA" >&2
              exit 1
            fi

            echo "[clone] repo:   $BUILD_REPO_URL"
            echo "[clone] commit: $BUILD_COMMIT_SHA"
            echo "[clone] dest:   $BUILD_WORKSPACE"

            # ── Credential helper ──────────────────────────────────────────────
            #
            # GIT_ASKPASS is a script git calls when it needs a username or
            # password. It receives a prompt string as $1 and prints the
            # appropriate value to stdout.
            #
            # This approach keeps credentials out of the remote URL (which
            # appears in git's process list and reflog) and out of any
            # .git/config file. The script lives in /tmp which is a tmpfs
            # mount — it is never written to the container image filesystem.
            #
            # The script is chmod 700 — readable and executable only by the
            # current user (UID 65532).
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

            # Disable the system credential store — we only want GIT_ASKPASS.
            export GIT_CONFIG_NOSYSTEM=1
            export HOME=/tmp

            # ── Safe directory ─────────────────────────────────────────────────────
            # The emptyDir volume is created by k8s as root:root. Git 2.35.2+
            # refuses to operate on directories not owned by the current user.
            # We register /workspace as safe before any git operation.
            # This is safe — /workspace is an isolated emptyDir, not a shared path.
            git config --global --add safe.directory "$BUILD_WORKSPACE"

            # ── Clone ──────────────────────────────────────────────────────────
            #
            # Strategy: clone with --no-checkout first so we don't pull the
            # working tree of the default branch (which may be large), then
            # fetch the specific commit if it isn't already present, then
            # checkout. This works even for commits that aren't on any branch
            # tip (e.g. a detached HEAD from a MR/PR).
            #
            # --filter=blob:none defers blob download until checkout, which
            # avoids transferring history blobs we will never use.
            mkdir -p "$BUILD_WORKSPACE"

            echo "[clone] cloning (partial)..."
            git clone \
              --no-checkout \
              --filter=blob:none \
              --depth=1 \
              "$BUILD_REPO_URL" \
              "$BUILD_WORKSPACE"

            cd "$BUILD_WORKSPACE"

            # Fetch the specific commit. --depth=1 gets just that commit
            # without any ancestor history.
            echo "[clone] fetching commit $BUILD_COMMIT_SHA..."
            git fetch \
              --depth=1 \
              origin \
              "$BUILD_COMMIT_SHA" \
              || {
                # Some git hosts (notably GitHub) do not allow fetching commits
                # by SHA directly unless uploadpack.allowReachableSHA1InWant is
                # enabled. Fall back to an unshallow fetch of the default branch
                # and then checkout the specific commit.
                echo "[clone] direct SHA fetch unsupported by host — fetching full default branch..."
                git fetch --unshallow origin
              }

            echo "[clone] checking out $BUILD_COMMIT_SHA..."
            git checkout "$BUILD_COMMIT_SHA"

            # ── Credential cleanup ─────────────────────────────────────────────
            #
            # Remove the askpass script immediately after the clone is complete.
            # This is belt-and-suspenders — the script only exists in /tmp
            # (tmpfs, not persisted) and the env vars are not inherited by the
            # pipeline container. But explicit cleanup is better than implicit.
            rm -f "$askpass_script"
            unset GIT_ASKPASS GIT_USERNAME GIT_PASSWORD

            # Strip the .git directory from the workspace.
            # The pipeline runner has no business running git commands and
            # the .git directory contains the remote URL with the host address.
            # Removing it also reduces workspace size for large repos.
            rm -rf "$BUILD_WORKSPACE/.git"

            echo "[clone] done — workspace ready at $BUILD_WORKSPACE"
          '';
        };

        # ── Passwd/group stubs ─────────────────────────────────────────────────
        # Required by the git binary's getpwuid() call at startup.
        passwdFile = pkgs.writeText "passwd" ''
          root:x:0:0:root:/root:/sbin/nologin
          cyclops:x:65532:65532:cyclops:/nonexistent:/sbin/nologin
        '';

        groupFile = pkgs.writeText "group" ''
          root:x:0:
          cyclops:x:65532:
        '';

        # ── Image ──────────────────────────────────────────────────────────────
        #
        # Attack surface is deliberately minimal:
        # - git binary and its runtime deps (libcurl, libssl via nixpkgs)
        # - the entrypoint script
        # - CA certificates for HTTPS clones
        # - no shell in PATH after entrypoint exec (writeShellApplication
        #   bundles bash only for the duration of the entrypoint)
        # - no package manager, no curl standalone, no ssh
        # - runs as UID 65532, never root
        # - read-only root filesystem; /tmp is a tmpfs mount at runtime
        cloneImage = pkgs.dockerTools.buildLayeredImage {
          name = "cyclops/git-clone";
          tag  = "latest";

          contents = [
            entrypoint
            pkgs.git
            pkgs.cacert
          ];

          extraCommands = ''
            mkdir -p etc tmp
            cp ${passwdFile} etc/passwd
            cp ${groupFile}  etc/group
            chmod 1777 tmp

            # git needs a HOME for its config. We point HOME=/tmp at runtime
            # so that .gitconfig writes go to tmpfs. Pre-create the directory
            # so there is no race on first use.
            mkdir -p tmp/git-home
          '';

          config = {
            Entrypoint = [ "${entrypoint}/bin/git-clone-entrypoint" ];
            Cmd        = [];
            User       = "65532:65532";

            # /workspace is always mounted as an emptyDir by the orchestrator.
            # /tmp is mounted as a tmpfs at runtime for credential scripts and
            # git config. Neither needs to be declared as a Volume — they are
            # injected by the Job manifest.
            Env = [
              # Ensure git uses the Nix CA bundle for HTTPS certificate
              # verification. Without this, git falls back to the system
              # bundle which does not exist in this minimal image.
              "GIT_SSL_CAINFO=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              # Suppress git's advice messages in log output.
              "GIT_TERMINAL_PROMPT=0"
            ];

            Labels = {
              "org.opencontainers.image.title"       = "cyclops-git-clone";
              "org.opencontainers.image.description" = "Init container for cloning source repos into Cyclops build jobs";
            };
          };

          maxLayers = 60;
        };

        # ── Push script ────────────────────────────────────────────────────────
        #
        # Builds the image, loads it, pushes it to your registry, and prints
        # the digest for pasting into cyclops.yaml as CLONE_INIT_IMAGE.
        #
        # Usage:
        #   nix run .#push -- registry.internal.example.com/cyclops/git-clone
        pushScript = pkgs.writeShellApplication {
          name = "push-clone-image";
          runtimeInputs = [ pkgs.docker pkgs.skopeo pkgs.coreutils ];
          text = ''
            set -euo pipefail

            TARGET="''${1:?Usage: push <registry/image:tag>}"

            echo "==> Building git-clone image..."
            IMAGE_ARCHIVE=$(nix build .#clone --no-link --print-out-paths)

            echo "==> Loading into Docker..."
            LOADED=$(docker load < "$IMAGE_ARCHIVE" | awk '{print $NF}')
            echo "    Loaded: $LOADED"

            echo "==> Tagging as $TARGET..."
            docker tag "$LOADED" "$TARGET"

            echo "==> Pushing..."
            docker push "$TARGET"

            DIGEST=$(docker inspect --format='{{index .RepoDigests 0}}' "$TARGET")
            echo ""
            echo "========================================================"
            echo " Push complete"
            echo "========================================================"
            echo ""
            echo " Digest: $DIGEST"
            echo ""
            echo " Paste into crates/cyclops-backend-k8s/src/job.rs:"
            echo ""
            echo '   const CLONE_INIT_IMAGE: &str = "'"$DIGEST"'";'
            echo ""
            echo "========================================================"
          '';
        };

      in {
        packages = {
          clone    = cloneImage;
          push     = pushScript;
          default  = cloneImage;
        };

        apps = {
          # Build and load into local Docker:
          #   nix run .#load
          load = {
            type    = "app";
            program = toString (pkgs.writeShellScript "load" ''
              set -euo pipefail
              IMAGE=$(nix build ${self}#clone --no-link --print-out-paths)
              docker load < "$IMAGE"
            '');
          };

          # Push to a registry and print the digest:
          #   nix run .#push -- registry.internal.example.com/cyclops/git-clone:latest
          push = {
            type    = "app";
            program = "${pushScript}/bin/push-clone-image";
          };

          # Stream directly without Docker daemon:
          #   nix run .#stream | skopeo copy docker-archive:/dev/stdin docker://...
          stream = {
            type    = "app";
            program = toString (pkgs.dockerTools.streamLayeredImage cloneImage);
          };

          default = {
            type    = "app";
            program = "${pushScript}/bin/push-clone-image";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ docker skopeo git ];
          shellHook = ''
            echo "cyclops/git-clone dev environment"
            echo ""
            echo "  nix run .#push -- <registry/image:tag>   build, push, print digest"
            echo "  nix run .#load                           build and load into Docker"
            echo "  nix run .#stream                         stream to stdout for skopeo"
            echo ""
          '';
        };
      }
    );
}
