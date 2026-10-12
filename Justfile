# Polar Justfile
#
# Host-side recipes: build images, run containers, dev workflow
# Container-side recipes: start llama-server, start pi, agent task management
#
# Usage:
#   just <recipe>                         # run with defaults
#   just platform=aarch64-linux <recipe>  # override platform
#   just verbose=false <recipe>           # quiet status line
#
# Platform autodetects from uname. Override for cross-builds.
# Verbose defaults to true (show full nix build logs).

platform := `echo "$(uname -m)-linux"`
verbose   := "true"

_nix_flags := if verbose == "true" { "-L" } else { "--log-format bar-with-logs" }

_taskfile := "/workspace/agent-task.json"

# ── Help ──────────────────────────────────────────────────────────────────────

# List all available recipes
default:
    @just --list

# ── Helpers (private) ─────────────────────────────────────────────────────────

# Load a docker-archive build result into the host podman.
# No-ops when podman is unavailable (e.g. inside the dev container, where
# podman-in-podman is explicitly not wanted and images reach the cluster via
# `just cluster-load-*` instead), or when POLAR_SKIP_LOAD is set. This lets the
# per-service build recipes run unchanged in both the dev container and on the
# host — in the container they build only; on the host they also load.
_load result:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${POLAR_SKIP_LOAD:-}" ]; then
        echo "POLAR_SKIP_LOAD set — skipping podman load of {{result}}"
        exit 0
    fi
    if ! command -v podman >/dev/null 2>&1; then
        echo "podman not found — skipping load of {{result}} (build-only)"
        exit 0
    fi
    podman load -i "{{result}}"

# ── Agent containers ──────────────────────────────────────────────────────────

# Build and load the agent container for your GPU type
# Usage: just agent-container           (ROCm, default)
#        just agent-container nvidia
#        just agent-container vulkan
#        just agent-container cpu
agent-container gpu='rocm':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{gpu}}" in
        rocm)   target=agentContainerRocm ;;
        nvidia) target=agentContainerNvidia ;;
        vulkan) target=agentContainerVulkan ;;
        cpu)    target=agentContainer ;;
        *) echo "Unknown gpu type: {{gpu}}. Use rocm, nvidia, vulkan, or cpu." && exit 1 ;;
    esac
    echo "Building agent container ($target) for {{platform}}..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.$target" -o result-agent-container
    just _load result-agent-container

# Build every package — binaries, all containers, TLS certs.
# Use this to verify the full build after disruptive changes.
build-all:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default"               -o result-binaries
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer"          -o result-dev-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.ciContainer"           -o result-ci-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainer"        -o result-agent-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerRocm"    -o result-agent-container-rocm
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerVulkan"  -o result-agent-container-vulkan
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniImage"          -o result-cassini
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniProducerImage"  -o result-harness-producer
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniSinkImage"      -o result-harness-sink
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitlabObserverImage"   -o result-gitlab-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitlabConsumerImage"   -o result-gitlab-consumer
    nix build {{_nix_flags}} ".#packages.{{platform}}.kubeObserverImage"     -o result-kube-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.kubeConsumerImage"     -o result-kube-consumer
    nix build {{_nix_flags}} ".#packages.{{platform}}.openApiObserverImage"  -o result-openapi-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.openApiProcessorImage" -o result-openapi-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.polarPkgs.ociResolver.resolverImage" -o result-resolver
    nix build {{_nix_flags}} ".#packages.{{platform}}.schedulerProcessorImage" -o result-scheduler-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.schedulerObserverImage"  -o result-scheduler-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.jiraObserverImage"     -o result-jira-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.jiraProcessorImage"    -o result-jira-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitObserverImage"      -o result-git-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitProcessorImage"     -o result-git-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitSchedulerImage"     -o result-git-scheduler
    nix build {{_nix_flags}} ".#packages.{{platform}}.credentialsAgentConsumerImage" -o result-credentials-agent-consumer
    nix build {{_nix_flags}} ".#packages.{{platform}}.credentialsAgentObserverImage" -o result-credentials-agent-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.buildProcessorImage"   -o result-build-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.nuInitImage"           -o result-nu-init
    nix build {{_nix_flags}} ".#packages.{{platform}}.polarInitImage"        -o result-polar-init
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitServerImage"        -o result-git-server

# ── Dev container ─────────────────────────────────────────────────────────────

# Build and load the dev container
dev-container:
    echo "Building dev container ({{platform}})..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer" -o result-dev-container
    just _load result-dev-container

# ── Local dev PKI / cert-issuer stack ─────────────────────────────────────────
#
# One-time bootstrap sequence (run once per CA rotation):
#
#   just setup-cert-issuer                    # generates CA, OIDC keypair, JWKS, tokens, config
#
# Then, in separate terminals (or tmux panes):
#
#   just run-jwks                     # Terminal A: JWKS HTTP server (port 8081)
#   just run-cert-issuer                   # Terminal B: cert-issuer gRPC server (port 8443)
#
# Then issue all three certs and copy Neo4j layout:
#
#   just issue-certs                    # issues cassini, neo4j, and agent certs
#
# Print env vars for a given workload:
#
#   just dev-env cassini
#   just dev-env agent
#   just dev-env neo4j
#
# Tokens expire after 1h. Refresh without rotating the CA:
#
#   just                     # safe to re-run; preserves dev/tmp/
#
# Rotate the CA root (invalidates all outstanding certs):
#
#   just dev-clean ca && just dev-setup

# Run the one-shot dev environment setup (CA, OIDC keypair, JWKS, tokens, config).
# Safe to re-run — preserves dev/tmp/. Delete dev/tmp/ explicitly to rotate the CA.
setup-cert-issuer:
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-issuer-setup

# Start the JWKS HTTP server on port 8081.
# Run in a dedicated terminal — this is a foreground process.
# Must be running before dev-issuer starts.
run-jwks:
    python3 -m http.server 8081 --directory dev

# Start the cert-issuer on port 8443.
# Run in a dedicated terminal — this is a foreground process.
# Requires dev-jwks to be running (issuer fetches JWKS on startup).
run-cert-issuer:
    export CERT_ISSUER_CONFIG=dev/config.json
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-issuer

# Issue all three dev certs (cassini server, neo4j server, agent client).
# Requires both dev-jwks and dev-issuer to be running in separate terminals.
# Tokens expire after 1h; re-run dev-setup first if they've expired.
issue-dev-certs:
    #!/usr/bin/env bash
    set -euo pipefail

    # TODO: add healthcheck
    # Fail fast if the issuer is not reachable rather than producing a confusing TLS error.
    # if ! curl -sf --max-time 2 http://127.0.0.1:8443 > /dev/null 2>&1; then
    #     echo "ERROR: cert-issuer not responding on :8443 — run 'just dev-issuer' in another terminal first."
    #     exit 1
    # fi

    echo "--- Issuing Cassini server cert ---"
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-client -- \
        --cert-issuer-url http://127.0.0.1:8443 \
        --token-path dev/token-cassini \
        --cert-dir dev/certs/server \
        --cert-type server \
        --key-algorithm ecdsa-p256

    echo "--- Issuing Neo4j server cert ---"
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-client -- \
        --cert-issuer-url http://127.0.0.1:8443 \
        --token-path dev/token-neo4j \
        --cert-dir dev/certs/neo4j-server \
        --cert-type server \
        --key-algorithm ecdsa-p256 \
        --extra-san neo4j.polar.svc.cluster.local \
        --extra-san polar-db-svc.polar.svc.cluster.local

    for p in https bolt; do
        mkdir -p "dev/certs/neo4j/$p/trusted"
        cp dev/certs/neo4j-server/cert.pem "dev/certs/neo4j/$p/public.crt"
        cp dev/certs/neo4j-server/key.pem  "dev/certs/neo4j/$p/private.key"
        cp dev/certs/neo4j-server/ca.pem   "dev/certs/neo4j/$p/trusted/ca.pem"
        echo "  populated dev/certs/neo4j/$p/"
    done

    echo "--- Issuing agent client cert ---"
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-client -- \
        --cert-issuer-url http://127.0.0.1:8443 \
        --token-path dev/token-agent \
        --cert-dir dev/certs/client \
        --key-algorithm ecdsa-p256

    echo "--- Verifying EKUs ---"
    for cert in dev/certs/server/cert.pem dev/certs/neo4j-server/cert.pem dev/certs/client/cert.pem; do
        echo "  $cert:"
        openssl x509 -in "$cert" -noout -text | grep -A5 'Extended Key Usage' | sed 's/^/    /'
    done

    echo "Done."

# ── Workspace binaries ────────────────────────────────────────────────────────

# Build all workspace binaries
agents:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default" -o polar

git-server:
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitServerImage" -o result-git-server-image
    just _load result-git-server-image

# Update the nix-container-lib pin in container-lib.dhall and re-render all container.nix files.
update-dhall-pins:
    #!/usr/bin/env bash
    set -euo pipefail
    COMMIT=$(nix flake metadata --json | jq -r '.locks.nodes["nix-container-lib"].locked.rev')
    DHALL_HASH=$(dhall hash <<< "https://raw.githubusercontent.com/daveman1010221/nix-container-lib/${COMMIT}/dhall/prelude.dhall")
    echo "nix-container-lib commit: $COMMIT"
    echo "Dhall prelude hash:       $DHALL_HASH"
    echo "Updating src/containers/container-lib.dhall..."
    echo "Re-rendering container.nix files..."
    for f in $(grep -rl "container-lib.dhall" src/ --include="*.dhall" | grep -v 'container-lib.dhall'); do
        DIR="$(dirname $f)"
        BASE="$(basename $f .dhall)"
        pushd "$DIR" > /dev/null
        dhall-to-nix < "$BASE.dhall" > "$BASE.nix" && echo "  rendered: $DIR/$BASE.nix" || echo "  skipped:  $f"
        popd > /dev/null
    done
    echo "Done. Review with: git diff"

# ── Run containers ────────────────────────────────────────────────────────────

# Start the agent container (AMD ROCm — RX 9070 XT)
run-rocm:
    podman run --rm -it --privileged --name polar-agent \
        --user 0 --userns=keep-id \
        --device /dev/kfd \
        --device /dev/dri/ \
        --ulimit memlock=-1:-1 \
        --security-opt=label=disable \
        --cap-add=SYS_PTRACE \
        --ipc=host \
        --network=host \
        -e CREATE_USER="$USER" \
        -e CREATE_UID="$(id -u)" \
        -e CREATE_GID="$(id -g)" \
        -e ATUIN_SESSION_NAME=polar-dev \
        -e HIP_VISIBLE_DEVICES=0 \
        -v /sys/class/kfd:/sys/class/kfd:ro \
        -v /sys/bus/pci/devices:/sys/bus/pci/devices:ro \
        -v ~/Documents/projects/ai_models/llama:/opt/llama-models:rw \
        -v ~/Documents/projects/ai_models/llama:/opt/ollama:rw \
        -v ~/Documents/projects/ai_state/pi:/opt/pi:rw \
        -v $PWD:/workspace \
        -v $HOME/.config/atuin:/root/.config/atuin:ro \
        -v $HOME/.local/share/atuin:/root/.local/share/atuin \
        localhost/polar-agent:latest

# Start the agent container (NVIDIA CUDA — RTX 4000 Ada)
run-nvidia:
    podman run --rm -it --privileged --name polar-agent \
        --user 0 --userns=keep-id \
        --device /dev/nvidia0 \
        --device /dev/nvidiactl \
        --device /dev/nvidia-uvm \
        --device /dev/nvidia-uvm-tools \
        --ulimit memlock=-1:-1 \
        --security-opt=label=disable \
        --cap-add=SYS_PTRACE \
        --ipc=host \
        --network=host \
        -e NVIDIA_VISIBLE_DEVICES=all \
        -e NVIDIA_DRIVER_CAPABILITIES=compute,utility \
        -e CREATE_USER="$USER" \
        -e CREATE_UID="$(id -u)" \
        -e CREATE_GID="$(id -g)" \
        -e ATUIN_SESSION_NAME=polar-dev \
        -e LD_LIBRARY_PATH=/run/opengl-driver/lib \
        -e OLLAMA_HOST="0.0.0.0:8080" \
        -v ~/Documents/projects/ai_models/llama:/opt/llama-models:rw \
        -v ~/Documents/projects/ai_models/ollama:/opt/ollama:rw \
        -v ~/Documents/projects/ai_state/pi:/opt/pi:rw \
        -v $PWD:/workspace \
        -v $HOME/.config/atuin:/root/.config/atuin:ro \
        -v $HOME/.local/share/atuin:/root/.local/share/atuin \
        -v `readlink -f /run/opengl-driver/lib/libcuda.so.1`:/run/opengl-driver/lib/libcuda.so.1:ro \
        -v `readlink -f /run/opengl-driver/lib/libcuda.so`:/run/opengl-driver/lib/libcuda.so:ro \
        -v `readlink -f /run/opengl-driver/lib/libnvidia-ml.so.1`:/run/opengl-driver/lib/libnvidia-ml.so.1:ro \
        localhost/polar-agent:latest

# Start the dev container
run-dev-container:
    podman run --rm --name polar-dev -it \
        --user 0 --userns=keep-id \
        -e CREATE_USER="$USER" \
        -e CREATE_UID="$(id -u)" \
        -e CREATE_GID="$(id -g)" \
        -e ATUIN_SESSION_NAME=polar-dev \
        -v $PWD:/workspace \
        -v $HOME/.config/atuin:/root/.config/atuin:ro \
        -v $HOME/.local/share/atuin:/root/.local/share/atuin \
        -p 2222:2223 \
        localhost/polar-dev:latest

# ── Inside container: LLM server ──────────────────────────────────────────────

# Start Qwen3-Coder-30B (AMD ROCm) — recommended
# Run in a separate terminal or tmux pane; does not background itself
llm-qwen:
    llama-server \
        --hf-repo unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF \
        --hf-file Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf \
        --ctx-size 65536 \
        --n-gpu-layers 32 \
        --flash-attn on \
        --alias "local-model" \
        --port 8080 --host 0.0.0.0 \
        --temperature 0.7 \
        --top-p 0.8 \
        --top-k 20 \
        --repeat-penalty 1.05 \
        --jinja \
        -ub 512 \
        --threads 20 \
        --threads-batch 20 \
        -ctk q4_0 -ctv q4_0 > /tmp/llama-server.log 2>&1

# Start OmniCoder-9B (AMD ROCm) — fallback for lighter tasks
llm-omnicoder-rocm-16GB:
    llama-server \
        --hf-repo Tesslate/OmniCoder-9B-GGUF \
        --hf-file omnicoder-9b-q8_0.gguf \
        --ctx-size 262144 \
        --n-gpu-layers 99 \
        --flash-attn on \
        --alias "local-model" \
        --port 8080 --host 0.0.0.0 \
        --temperature 0.3 \
        --top-p 0.95 \
        --top-k 20 \
        --min-p 0.0 \
        --repeat-penalty 1.0 \
        --jinja \
        --spec-type ngram-mod \
        -ub 512 \
        -ctk q8_0 -ctv q8_0 > /tmp/llama-server.log 2>&1

llm-omnicoder-nvidia-12GB:
    llama-server \
        --hf-repo Tesslate/OmniCoder-9B-GGUF \
        --hf-file omnicoder-9b-q8_0.gguf \
        --ctx-size 131072 \
        --n-gpu-layers 99 \
        --flash-attn on \
        --alias "local-model" \
        --port 8080 --host 0.0.0.0 \
        --temperature 0.3 \
        --top-p 0.95 \
        --top-k 20 \
        --min-p 0.0 \
        --repeat-penalty 1.0 \
        --jinja \
        --spec-type ngram-mod \
        -ub 512 \
        -ctk q8_0 -ctv q8_0 > /tmp/llama-server.log 2>&1

# ── Inside container: Pi agent ────────────────────────────────────────────────

pi_version := ""

# Download/update pi to latest version
pi-install:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(curl -fsSL https://api.github.com/repos/badlogic/pi-mono/releases/latest | grep '"tag_name"' | sed 's/.*"v\([^"]*\)".*/\1/')
    INSTALLED=""
    if [ -f ~/pi/pi ]; then
        INSTALLED=$(~/pi/pi --version 2>/dev/null | grep -oP '\d+\.\d+\.\d+' || echo "")
    fi
    if [ "$INSTALLED" = "$VERSION" ]; then
        echo "pi v${VERSION} already installed, skipping."
        exit 0
    fi
    echo "Installing pi v${VERSION}..."
    curl -fsSL "https://github.com/badlogic/pi-mono/releases/download/v${VERSION}/pi-linux-x64.tar.gz" \
        | tar -xz -C ~
    echo "pi v${VERSION} installed."

# Launch pi (installs/updates first)
pi: pi-install
    ~/pi/pi @/workspace/agent-task.json

# ── Agent task management (container-side) ────────────────────────────────────

# Show current agent task status
agent-status:
    #!/usr/bin/env bash
    if [ ! -f {{_taskfile}} ]; then
        echo "No task file found at {{_taskfile}}"
        exit 1
    fi
    echo "=== Current Task ==="
    jq '.current_task' {{_taskfile}}
    echo ""
    echo "=== Pending ($(jq '.pending | length' {{_taskfile}}) tasks) ==="
    jq '.pending[] | {crate, op, review}' {{_taskfile}}
    echo ""
    echo "=== Completed ($(jq '.completed | length' {{_taskfile}}) tasks) ==="
    jq '.completed[] | {crate, op, status}' {{_taskfile}}

# Resume agent from existing task file — prints the resume prompt to paste into pi
agent-resume:
    #!/usr/bin/env bash
    if [ ! -f {{_taskfile}} ]; then
        echo "No task file found at {{_taskfile}}"
        exit 1
    fi
    WORKSPACE_ROOT=$(jq -r '.workspace_root' {{_taskfile}})
    CRATE=$(jq -r '.current_task.crate' {{_taskfile}})
    OP=$(jq -r '.current_task.op' {{_taskfile}})
    echo "Paste this into pi to resume:"
    echo "────────────────────────────────────────────────────────────────"
    printf "export TASKFILE=%s WORKSPACE_ROOT=%s\n" "{{_taskfile}}" "$WORKSPACE_ROOT"
    printf "Read your task file: jq '.' \$TASKFILE\n"
    printf "Your current task is .current_task — crate=%s op=%s\n" "$CRATE" "$OP"
    printf "Your tools are in .tools — substitute {placeholders} as needed. Use locate tools first to find symbols, then extract tools to read exact definitions.\n"
    printf "Begin: cd \$WORKSPACE_ROOT && cargo check --package %s 2>&1 | head -80\n" "$CRATE"
    printf "Follow .current_task.next_action. Update .current_task.current_file as you move between files. When .current_task.success_condition is met run the taskfile.mark_done tool to advance to the next pending task. Do not ask for clarification. Start working.\n"
    echo "────────────────────────────────────────────────────────────────"

# Set agent to work on a specific crate and op, or pop next pending task
# Usage: just agent-task                        # pop next pending, launch pi
#        just agent-task cassini-client write-tests  # queue explicit task, launch pi
agent-task crate='' op='':
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f {{_taskfile}} ]; then
        echo "No task file found at {{_taskfile}}"
        exit 1
    fi

    if [ -z "{{crate}}" ]; then
        # No args — pop first pending task into current_task
        PENDING_LEN=$(jq '.pending | length' {{_taskfile}})
        if [ "$PENDING_LEN" -eq 0 ]; then
            echo "No pending tasks. Add tasks to pending in {{_taskfile}} first."
            exit 1
        fi
        jq '
            .current_task = .pending[0] |
            .pending = .pending[1:]
        ' {{_taskfile}} > {{_taskfile}}.tmp && mv {{_taskfile}}.tmp {{_taskfile}}
    else
        # Explicit crate/op — push current_task to back of pending if one exists
        jq --arg crate "{{crate}}" --arg op "{{op}}" '
            if .current_task != null then
                .pending = .pending + [.current_task]
            else
                .
            end |
            .current_task = {
                "crate": $crate,
                "op": $op,
                "status": "in-progress"
            }
        ' {{_taskfile}} > {{_taskfile}}.tmp && mv {{_taskfile}}.tmp {{_taskfile}}
    fi

    echo "=== Current Task ==="
    jq '.current_task' {{_taskfile}}
    echo ""

    # Launch pi
    just pi

# ── Dev workflow ──────────────────────────────────────────────────────────────

# Check all crates compile
check:
    cargo check --workspace

# Run tests — optionally scoped to a single package
# Usage: just test
#        just test cassini-types
test package='':
    #!/usr/bin/env bash
    if [ -n "{{package}}" ]; then
        cargo test --package {{package}} -- --nocapture
    else
        cargo test --workspace -- --nocapture
    fi

# Run clippy with warnings as errors
lint:
    cargo clippy --workspace -- -D warnings

# Format all crates
fmt:
    cargo fmt --all

# Run static analysis tools
static-analysis:
    sh scripts/static-tools.sh --manifest-path src/agents/Cargo.toml

# Run CI pipeline locally
ci:
    podman run --rm \
        --name polar-dev \
        --env-file=ci.env \
        --env CI_COMMIT_REF_NAME=$(git symbolic-ref --short HEAD) \
        --env CI_COMMIT_SHORT_SHA=$(git rev-parse --short=8 HEAD) \
        --env SSL_CERT_FILE="./src/conf/certs/proxy-ca.pem" \
        --user 0 \
        --userns=keep-id \
        -v $(pwd):/workspace:rw \
        -p 2222:2223 \
        polar-dev:0.1.0 \
        bash -c "start.sh; chmod +x scripts/gitlab-ci.sh; chmod +x scripts/static-tools.sh; scripts/gitlab-ci.sh"

# Import your claimed user certificate (and the Polar CA) into Librewolf.
# Reads ~/.config/polar-admin/<username>.{crt,key} and ca.pem, which `claim`
# and `bootstrap` wrote. Close Librewolf first so the NSS db is not locked.
#
# First removes the old Polar certificates (with their private keys) from the
# store: a new cluster has a new CA, so the old CA and every client certificate
# it issued are dead and would otherwise stack up. A certificate counts as old
# if its nickname starts with "Polar ", or its subject is this user's or the
# CA's, or its issuer is the CA's name. Matching is by subject and issuer, not
# nickname alone, because NSS gives a new certificate for the same subject the
# old one's nickname (usually "Imported Certificate"). Nothing else is touched.
#
# Usage: just user-cert-install            (defaults to djshepard)
#        just user-cert-install alice
# POLAR_NSS_DB=<profile dir> overrides which profile is used (for testing on a copy).
user-cert-install username="djshepard":
    #!/usr/bin/env bash
    set -euo pipefail
    DIR="$HOME/.config/polar-admin"
    for f in "{{username}}.crt" "{{username}}.key" "ca.pem"; do
        [ -f "$DIR/$f" ] || { echo "ERROR: missing $DIR/$f" >&2; exit 1; }
    done
    # Librewolf keeps profiles in ~/.librewolf, or under ~/.config/librewolf/librewolf on
    # newer versions and some packagings. The first profile found is used; with several, set
    # POLAR_NSS_DB.
    NSS_DB="${POLAR_NSS_DB:-$({ find "$HOME/.librewolf" "$HOME/.config/librewolf" -maxdepth 3 -name cert9.db -printf '%h\n' 2>/dev/null || true; } | head -1)}"
    [ -n "$NSS_DB" ] || { echo "ERROR: no Librewolf profile with cert9.db found under ~/.librewolf or ~/.config/librewolf (set POLAR_NSS_DB to the profile directory)" >&2; exit 1; }
    # Firefox-family browsers keep a `lock` symlink in the profile while running,
    # named "<ip>:+<pid>". A crash leaves it behind, so only a live pid counts.
    if [ -L "$NSS_DB/lock" ]; then
        lock_pid=$(readlink "$NSS_DB/lock"); lock_pid=${lock_pid##*+}
        if [[ "$lock_pid" =~ ^[0-9]+$ ]] && kill -0 "$lock_pid" 2>/dev/null; then
            echo "ERROR: Librewolf is running (process $lock_pid holds the profile lock). Close it and re-run." >&2
            exit 1
        fi
        echo "NOTE: ignoring a stale profile lock (process $lock_pid is gone)."
    fi
    # Not named TMP: nix-shell's stdenv overwrites TMP and TMPDIR inside the shell.
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT
    openssl pkcs12 -export \
        -inkey "$DIR/{{username}}.key" \
        -in "$DIR/{{username}}.crt" \
        -certfile "$DIR/ca.pem" \
        -name "Polar {{username}}" \
        -out "$WORK/user.p12" \
        -passout pass:changeit
    # Runs inside nix-shell (needs certutil/pk12util). A file, not an inline
    # string, so its quoting stays readable.
    cat > "$WORK/install.sh" <<'EOS'
    set -euo pipefail
    db="sql:$NSS_DB"

    # NSS gives every certificate with the same subject the same nickname, so a
    # new certificate for a user who already has one in the store silently
    # inherits the old nickname (typically "Imported Certificate") and the two
    # stack up. So old certificates are recognised by what they are, not by
    # name: compare subject and issuer hashes of the raw certificate.
    user_hash=$(openssl x509 -in "$DIR/$POLAR_USER.crt" -noout -subject_hash)
    ca_hash=$(openssl x509 -in "$DIR/ca.pem" -noout -subject_hash)

    removed=0
    # True if a private key is stored under this nickname. `certutil -K` lines
    # look like "< 0> ec  <key id>  <nickname>": drop the first four fields.
    has_key() {
        certutil -K -d "$db" 2>/dev/null \
            | awk -v n="$1" '/^</ { $1 = $2 = $3 = $4 = ""; sub(/^ +/, ""); if ($0 == n) found = 1 } END { exit !found }'
    }
    # Deletes every certificate stored under a nickname, and the private keys
    # that go with them. One at a time, because they can differ: -F removes a
    # certificate together with its key, but quietly does nothing for one that
    # has no key (a CA), and -D removes a certificate but would leave a key behind.
    remove_all() {
        local nick="$1" guard=0
        while certutil -L -n "$nick" -d "$db" >/dev/null 2>&1 && [ "$guard" -lt 20 ]; do
            if has_key "$nick"; then
                certutil -F -n "$nick" -d "$db" 2>/dev/null || true
            else
                certutil -D -n "$nick" -d "$db" 2>/dev/null || true
            fi
            guard=$((guard + 1))
            removed=$((removed + 1))
        done
    }

    echo "Removing old Polar certificates from $NSS_DB ..."
    # One line per nickname: the listing minus its trailing trust flags
    # ("u,u,u", "CT,,"), which are padded with spaces.
    certutil -L -d "$db" \
        | awk 'NF && !/^Certificate Nickname/ && !/^[[:space:]]*SSL,S\/MIME/ { sub(/[[:space:]]+$/, ""); sub(/[[:space:]]+[A-Za-z,]*$/, ""); print }' \
        | sort -u > "$WORK/nicknames"
    while IFS= read -r nick; do
        [ -n "$nick" ] || continue
        # Raw DER of one certificate under this nickname; all of them share a subject.
        certutil -L -n "$nick" -d "$db" -r > "$WORK/probe.der" 2>/dev/null || continue
        subject_hash=$(openssl x509 -inform DER -in "$WORK/probe.der" -noout -subject_hash 2>/dev/null || true)
        issuer_hash=$(openssl x509 -inform DER -in "$WORK/probe.der" -noout -issuer_hash 2>/dev/null || true)
        if [[ "$nick" == "Polar "* ]] \
            || [ "$subject_hash" = "$user_hash" ] \
            || [ "$subject_hash" = "$ca_hash" ] \
            || [ "$issuer_hash" = "$ca_hash" ]; then
            echo "  removing: $nick"
            remove_all "$nick"
        fi
    done < "$WORK/nicknames"
    echo "  removed $removed certificate(s)"

    echo "Importing into $NSS_DB ..."
    pk12util -i "$WORK/user.p12" -d "$db" -W changeit
    # pk12util imports the CA from the bundle without trust; replace it with a trusted one.
    certutil -D -n 'Polar Internal CA' -d "$db" 2>/dev/null || true
    certutil -A -n 'Polar Internal CA' -t 'CT,,' -a -i "$DIR/ca.pem" -d "$db"
    EOS
    export NSS_DB DIR WORK POLAR_USER="{{username}}"
    nix-shell -p nssTools openssl --run "bash $WORK/install.sh"
    echo "Done. just port-forwards-up, then browse to https://localhost:7473"

# ── polar-admin-grant wrappers ───────────────────────────────────────────────
#
# The observer URL is the port-forward from `just port-forwards-up`. Override
# for another cluster:  POLAR_OBSERVER_URL=https://... just admin-claim ...
# (product code has no default; this is only the dev convenience.)

_admin_dir    := env_var('HOME') + "/.config/polar-admin"
_observer_url := env_var_or_default('POLAR_OBSERVER_URL', 'http://localhost:8081')

# Step 1: fetch the CA, then redeem the one-shot bootstrap credential.
# Writes ~/.config/polar-admin/{ca.pem,bootstrap-admin.crt,bootstrap-admin.key}.
admin-bootstrap:
    mkdir -p {{_admin_dir}}
    chmod 700 {{_admin_dir}}
    kubectl -n polar get configmap polar-ca -o jsonpath='{.data.ca\.pem}' > {{_admin_dir}}/ca.pem
    cd src/agents && cargo run -q -p polar-admin-grant -- \
        --observer-url {{_observer_url}} bootstrap \
        --cert-out {{_admin_dir}}/bootstrap-admin.crt \
        --key-out  {{_admin_dir}}/bootstrap-admin.key

# Step 2: provision a normal admin account, signed with the bootstrap cert.
# The account can provision accounts and grant permissions, and sign the two kinds of document the
# agents act on: a configuration (c:update-configuration) and a command (c:run-command). Capabilities
# are fixed when the certificate is issued, so an account provisioned before one was added to this
# list does not have it; provision another, or reissue.
# The bootstrap certificate is spent by the first provisioning, so a later account is provisioned
# by an account that already holds c:provision-account: name it as the signer.
# Usage: just admin-provision djshepard djshepard@sei.cmu.edu
#        just admin-provision operator ops@example.org djshepard
admin-provision username email signer="bootstrap-admin":
    cd src/agents && cargo run -q -p polar-admin-grant -- \
        --cert {{_admin_dir}}/{{signer}}.crt \
        --key  {{_admin_dir}}/{{signer}}.key \
        --observer-url {{_observer_url}} provision-account \
        --username {{username}} --email {{email}} \
        --permission p:u2s:polar-db-svc.polar-graph.svc.cluster.local \
        --permission p:u2s:credentials-agent-observer.polar.svc.cluster.local \
        --capability c:provision-account --capability c:grant \
        --capability c:update-configuration --capability c:run-command \
        --capability c:update-policy --capability c:update-runtime \
        --capability c:update-groups \
        --reason user-onboarding

# Step 3: claim the provisioned account. Generates the keypair locally (the
# key never leaves this machine) and writes ~/.config/polar-admin/<username>.{crt,key}.
# Usage: just admin-claim djshepard djshepard@sei.cmu.edu
admin-claim username email:
    cd src/agents && cargo run -q -p polar-admin-grant -- \
        --observer-url {{_observer_url}} claim \
        --username {{username}} --email {{email}} \
        --cert-out {{_admin_dir}}/{{username}}.crt \
        --key-out  {{_admin_dir}}/{{username}}.key

# Load a neo4j dump file into the cluster database.
# Scales down neo4j, runs a loader pod as root, chowns the data, scales back up,
# and resets the neo4j password to match the cluster secret.
# Usage: just neo4j-load-dump ~/Documents/projects/jira.5.26.26.neo4j.dump
neo4j-load-dump dump_path:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "{{dump_path}}" ]; then
        echo "ERROR: dump file not found: {{dump_path}}"
        exit 1
    fi
    NEO4J_PASSWORD=$(kubectl get secret -n polar-graph neo4j-secret -o jsonpath='{.data.secret}' | base64 -d | cut -d'/' -f2)
    echo "Scaling neo4j down..."
    kubectl scale statefulset -n polar-graph polar-neo4j --replicas=0
    kubectl wait --for=delete pod/polar-neo4j-0 -n polar-graph --timeout=120s 2>/dev/null || true
    echo "Launching loader pod..."
    kubectl run -n polar-graph neo4j-loader \
        --image=neo4j:5.26.2 \
        --restart=Never \
        --overrides='{"spec":{"securityContext":{"runAsUser":0,"runAsGroup":0},"volumes":[{"name":"data","persistentVolumeClaim":{"claimName":"polar-db-data"}},{"name":"logs","persistentVolumeClaim":{"claimName":"polar-db-logs"}}],"containers":[{"name":"neo4j-loader","image":"neo4j:5.26.2","command":["/bin/sh","-c","sleep 3600"],"volumeMounts":[{"name":"data","mountPath":"/var/lib/neo4j/data"},{"name":"logs","mountPath":"/var/lib/neo4j/logs"}]}]}}'
    kubectl wait --for=condition=Ready pod/neo4j-loader -n polar-graph --timeout=60s
    echo "Copying dump file..."
    cat "{{dump_path}}" | kubectl exec -i -n polar-graph neo4j-loader -- /bin/sh -c 'cat > /var/lib/neo4j/data/neo4j.dump'
    echo "Loading dump..."
    kubectl exec -n polar-graph neo4j-loader -- /bin/sh -c \
        'mkdir -p /var/lib/neo4j/tmp && NEO4J_HOME=/var/lib/neo4j NEO4J_CONF=/var/lib/neo4j/conf JAVA_TOOL_OPTIONS="-Djava.io.tmpdir=/var/lib/neo4j/tmp" /nix/store/qbds1qr8sf5qkzz8cady90bwhcnw05xf-neo4j-community-src-2025.8.0/opt/neo4j/bin/neo4j-admin database load neo4j --from-path=/var/lib/neo4j/data --overwrite-destination=true'
    echo "Fixing ownership..."
    kubectl exec -n polar-graph neo4j-loader -- /bin/sh -c 'chown -R 7474:7474 /var/lib/neo4j/data && rm /var/lib/neo4j/data/neo4j.dump'
    echo "Cleaning up loader pod..."
    kubectl delete pod -n polar-graph neo4j-loader
    echo "Scaling neo4j back up..."
    kubectl scale statefulset -n polar-graph polar-neo4j --replicas=1
    echo "Waiting for neo4j to be ready..."
    kubectl wait --for=condition=Ready pod/polar-neo4j-0 -n polar-graph --timeout=180s
    echo "Waiting for neo4j to finish initializing..."
    sleep 15
    echo "Resetting neo4j password..."
    kubectl exec -n polar-graph polar-neo4j-0 -- /nix/store/qbds1qr8sf5qkzz8cady90bwhcnw05xf-neo4j-community-src-2025.8.0/opt/neo4j/bin/cypher-shell \
        -u neo4j -p neo4j \
        "ALTER USER neo4j SET PASSWORD '$NEO4J_PASSWORD' CHANGE NOT REQUIRED" || \
    kubectl exec -n polar-graph polar-neo4j-0 -- /nix/store/qbds1qr8sf5qkzz8cady90bwhcnw05xf-neo4j-community-src-2025.8.0/opt/neo4j/bin/cypher-shell \
        -u neo4j -p "$NEO4J_PASSWORD" \
        "ALTER USER neo4j SET PASSWORD '$NEO4J_PASSWORD' CHANGE NOT REQUIRED"
    echo "Done. If you changed users, re-run: just user-cert-install"

# Build the nix images (archives under result/). Needs only nix: no registry, no cluster.
# With no names, builds every image.  just images-build kube-observer
images-build *names:
    nu infra/images.nu build {{ if names == "" { "--all" } else { names } }}

# Build and push to the local registry. Needs the registry, not the cluster.
images-push *names: registry-up
    nu infra/images.nu push {{ if names == "" { "--all" } else { names } }}

# Copy third-party images from upstream into the local registry.
images-mirror *names: registry-up
    nu infra/images.nu mirror {{ if names == "" { "--all" } else { names } }}

# Every stage for the named images, then drop the cluster node's stale copies.
#   just image kube-observer jira-observer
image +names: registry-up
    nu infra/images.nu sync {{names}}

# Every stage for every image in infra/images.nuon.
images-all: registry-up
    nu infra/images.nu sync --all

# ── nix-usernetes (local cluster) ────────────────────────────────────────────
#
# Full startup sequence. Steps marked [dev] are for developers using
# nix-usernetes locally; users bringing their own cluster skip those and
# need their images in a registry the cluster can pull from.
#
#   1. [dev] Fresh cluster
#        cd ~/Documents/projects/nix-usernetes
#        just reset && just up && just init && just kubeconfig
#        set -gx KUBECONFIG (pwd)/kubeconfig      # fish
#        cd ~/Documents/projects/Polar
#
#   2. Bring it up, from the Polar repo root with KUBECONFIG set. --dev builds and
#      loads images into the local registry; only the steps the target needs run:
#        nu infra/up.nu local-minimal --dev --user <user> --email <email>
#      It renders, applies namespaces, bootstraps the CA, applies the manifests,
#      waits for the core pods, starts the needed port-forwards and runs the admin
#      flow. It pauses for the browser import and the Neo4j password, and is safe
#      to re-run to resume. The individual recipes (admin-bootstrap, admin-provision,
#      admin-claim, user-cert-install, port-forwards-up) still work on their own.

_u7s_dir      := env_var('HOME') + "/Documents/projects/nix-usernetes"
_u7s_kubeconf := _u7s_dir + "/kubeconfig"

# Wipe all cluster state
cluster-reset:
    just -f {{_u7s_dir}}/Justfile reset

# Delete the pods that failed. A Job with restartPolicy Never and a large backoffLimit keeps every
# failed pod for as long as it runs, so an agent that cannot start (the Jira observer with no Jira
# to reach) leaves hundreds in a weekend, and their logs and records fill the node until it taints
# itself with disk pressure and nothing schedules. Safe to run at any time: running pods are untouched.
cluster-prune-failed:
    kubectl delete pods -A --field-selector=status.phase=Failed --wait=false

# Show cluster status. Respects whatever KUBECONFIG is already set.
cluster-status:
    kubectl get nodes
    kubectl get pods -A

# Start detached, auto-restarting port-forwards for local dev (neo4j
# browser/bolt, keycloak admin console, ace dashboard, jaeger traces/UI,
# credentials-agent observer). Fully detached via setsid -- survives
# terminal close, does not appear in `jobs`. Logs go to /tmp/pf-*.log; stop
# with `just port-forwards-down`.
#
# The observer is on local 8081, not 8080: keycloak already owns 8080, and
# a forward that loses that race silently talks to the wrong service.
port-forwards-up *only:
    #!/usr/bin/env bash
    set -euo pipefail
    # No arguments: start every forward. Otherwise only the named ones
    # (neo4j keycloak ace jaeger observer git).
    ONLY=" {{only}} "
    want() { [ "$ONLY" = "  " ] || [[ "$ONLY" == *" $1 "* ]]; }
    start_pf() {
        local name="$1" ns="$2" svc="$3" ports="$4" logfile="$5"
        want "$name" || return 0
        setsid bash -c "while true; do kubectl port-forward -n $ns svc/$svc $ports > $logfile 2>&1; sleep 1; done" < /dev/null > /dev/null 2>&1 &
        disown
        echo "  forward started: $name ($ports)"
    }
    start_pf neo4j    polar-graph polar-db-svc                 "7473:7473 7687:7687"   /tmp/pf-neo4j.log
    start_pf keycloak polar-auth  keycloak-svc                 "8080:8080"             /tmp/pf-keycloak.log
    start_pf ace      polar       ace-svc                      "8050:8050"             /tmp/pf-ace.log
    start_pf jaeger   polar       jaeger-svc                   "4318:4318 16686:16686" /tmp/pf-jaeger.log
    start_pf observer polar       credentials-agent-observer   "8081:8080"             /tmp/pf-credentials-observer.log
    start_pf git      polar       polar-git-server             "8443:8443"             /tmp/pf-polar-git-server.log
    echo "Logs: /tmp/pf-*.log"

# The polar-git-server, as the git CLI reaches it from this machine: the cluster's own name for it,
# which is the name on its certificate, mapped to the port-forward (just port-forwards-up git).
# git checks the certificate's name against the URL's host, so the URL keeps the cluster name and
# curl is told where that name lives (http.curloptResolve, git 2.37 or newer).
_git_host := "polar-git-server.polar.svc.cluster.local"

# The git configuration that makes `git` present your Polar certificate to the Polar git server.
# Prints it; changes nothing. GIT_SSL_CAINFO in the environment overrides http.sslCAInfo, so unset
# it (in fish: set -e GIT_SSL_CAINFO) or use `just polar-git-clone`, which does it for one command.
# Usage: just polar-git-config            (as djshepard)
#        just polar-git-config alice
polar-git-config user="djshepard":
    #!/usr/bin/env bash
    set -euo pipefail
    DIR="$HOME/.config/polar-admin"
    for f in "{{user}}.crt" "{{user}}.key" "ca.pem"; do
        [ -f "$DIR/$f" ] || { echo "ERROR: missing $DIR/$f" >&2; exit 1; }
    done
    U="https://{{_git_host}}:8443/"
    echo "git config --global 'http.$U.sslCAInfo' $DIR/ca.pem"
    echo "git config --global 'http.$U.sslCert' $DIR/{{user}}.crt"
    echo "git config --global 'http.$U.sslKey' $DIR/{{user}}.key"
    echo "git config --global 'http.$U.curloptResolve' {{_git_host}}:8443:127.0.0.1"
    echo
    echo "Then: git clone ${U}polar-schedules.git"
    if [ -n "${GIT_SSL_CAINFO:-}" ]; then
        echo "NOTE: GIT_SSL_CAINFO is set ($GIT_SSL_CAINFO) and overrides http.sslCAInfo." >&2
    fi

# Clone a repository from the Polar git server with your Polar certificate, for this one command,
# without touching your git configuration. Needs the port-forward (just port-forwards-up git).
# Usage: just polar-git-clone polar-schedules
#        just polar-git-clone polar-schedules ~/work/schedules alice
polar-git-clone repo dest="" user="djshepard":
    #!/usr/bin/env bash
    set -euo pipefail
    DIR="$HOME/.config/polar-admin"
    for f in "{{user}}.crt" "{{user}}.key" "ca.pem"; do
        [ -f "$DIR/$f" ] || { echo "ERROR: missing $DIR/$f" >&2; exit 1; }
    done
    env -u GIT_SSL_CAINFO -u SSL_CERT_FILE git \
        -c "http.sslCAInfo=$DIR/ca.pem" \
        -c "http.sslCert=$DIR/{{user}}.crt" \
        -c "http.sslKey=$DIR/{{user}}.key" \
        -c "http.curloptResolve={{_git_host}}:8443:127.0.0.1" \
        clone "https://{{_git_host}}:8443/{{repo}}.git" {{dest}}

# Ask the Cassini broker in the cluster to shut down in order, over the wire, with the shutdown token
# from its Secret and your certificate (just user-cert-install). The Job starts a new broker.
# Usage: just cassini-shutdown            (as djshepard)
#        just cassini-shutdown alice
cassini-shutdown user="djshepard":
    nu infra/cassini-shutdown.nu --user {{user}}

# Stop all detached port-forwards started by port-forwards-up.
port-forwards-down:
    #!/usr/bin/env bash
    pkill -f "port-forward -n polar-graph svc/polar-db-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar-auth svc/keycloak-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/ace-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/jaeger-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/credentials-agent-observer" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/polar-git-server" 2>/dev/null || true
    echo "Port-forwards stopped."

# Idempotent Keycloak bootstrap: realm, public client (PKCE, Authorization
# Code + direct-grant enabled for scripted testing), audience mapper
# scoping tokens to cert-issuer, and a test user. Safe to re-run --
# every step checks for existence first. Test user credentials are read
# directly from sops (secrets.sops.yaml / user.sops.yaml), never stored
# as a Kubernetes Secret -- nothing in the cluster ever consumes them,
# so there's no reason for them to touch cluster storage at all.
#
# Known Keycloak gotchas this script works around (found the hard way):
#   - kcadm set-password defaults to a *temporary* password, which
#     attaches a required action and blocks direct-grant login entirely
#     until resolved -- creating the user with an inline credentials
#     array (temporary=false) sidesteps this in one step.
#   - the realm's default User Profile requires email/firstName/lastName;
#     a user missing any of these fails direct-grant with the generic,
#     unhelpful "Account is not fully set up" error, indistinguishable
#     from the temporary-password case without checking both.
#   - kcadm's `-q` search does a *partial* match by default (e.g.
#     username=testuser also matches "testuser2") -- every existence
#     check below passes `-q exact=true` explicitly to avoid a false
#     "already exists" against an unrelated, similarly-named resource.
keycloak-bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail

    KC_NS="polar-auth"
    KC_URL="http://localhost:8080"
    KC_REALM="polar"
    KC_CLIENT_ID="browser-cert-fetcher"
    KC_AUDIENCE="polar-cert-issuer.local"
    KC_REDIRECT_URI="http://localhost:8765/callback"

    kc() {
        kubectl exec -n "$KC_NS" deployment/keycloak -- /opt/keycloak/bin/kcadm.sh "$@"
    }

    echo "Logging into kcadm..."
    KC_ADMIN_USER=$(kubectl get secret -n "$KC_NS" keycloak-admin-secret -o jsonpath="{.data.username}" | base64 -d)
    KC_ADMIN_PASSWORD=$(kubectl get secret -n "$KC_NS" keycloak-admin-secret -o jsonpath="{.data.password}" | base64 -d)
    kc config credentials --server "$KC_URL" --realm master --user "$KC_ADMIN_USER" --password "$KC_ADMIN_PASSWORD"

    echo "Ensuring realm '$KC_REALM' exists..."
    REALM_CHECK=$(kc get "realms/$KC_REALM" 2>/dev/null || echo "{}")
    if [ "$(echo "$REALM_CHECK" | jq -r ".realm // empty")" != "$KC_REALM" ]; then
        kc create realms -s realm="$KC_REALM" -s enabled=true
        echo "  created"
    else
        echo "  already exists"
    fi

    echo "Ensuring client '$KC_CLIENT_ID' exists..."
    CLIENT_LIST=$(kc get clients -r "$KC_REALM" -q clientId="$KC_CLIENT_ID" -q exact=true 2>/dev/null || echo "[]")
    CLIENT_ID_INTERNAL=$(echo "$CLIENT_LIST" | jq -r ".[0].id // empty")
    if [ -z "$CLIENT_ID_INTERNAL" ]; then
        kc create clients -r "$KC_REALM" \
            -s clientId="$KC_CLIENT_ID" \
            -s publicClient=true \
            -s standardFlowEnabled=true \
            -s directAccessGrantsEnabled=true \
            -s "redirectUris=[\"$KC_REDIRECT_URI\"]" \
            -s "attributes={\"pkce.code.challenge.method\":\"S256\"}"
        CLIENT_ID_INTERNAL=$(kc get clients -r "$KC_REALM" -q clientId="$KC_CLIENT_ID" -q exact=true | jq -r ".[0].id")
        echo "  created (id=$CLIENT_ID_INTERNAL)"
    else
        echo "  already exists (id=$CLIENT_ID_INTERNAL)"
    fi

    echo "Ensuring audience mapper exists..."
    MAPPERS=$(kc get "clients/$CLIENT_ID_INTERNAL/protocol-mappers/models" -r "$KC_REALM" 2>/dev/null || echo "[]")
    MAPPER_EXISTS=$(echo "$MAPPERS" | jq -r ".[] | select(.name==\"polar-audience\") | .id // empty")
    if [ -z "$MAPPER_EXISTS" ]; then
        kc create "clients/$CLIENT_ID_INTERNAL/protocol-mappers/models" -r "$KC_REALM" \
            -s name=polar-audience \
            -s protocol=openid-connect \
            -s protocolMapper=oidc-audience-mapper \
            -s "config={\"included.custom.audience\":\"$KC_AUDIENCE\",\"id.token.claim\":\"false\",\"access.token.claim\":\"true\"}"
        echo "  created"
    else
        echo "  already exists"
    fi

    echo "Ensuring test user exists..."
    TARGET_DIR="infra/targets/local-minimal"
    SHARED_JSON=$(SOPS_CONFIG="$TARGET_DIR/.sops.yaml" sops --decrypt --output-type json "$TARGET_DIR/secrets.sops.yaml" 2>/dev/null || echo "{}")
    USER_JSON=$(SOPS_CONFIG="$TARGET_DIR/.user.sops.yaml" sops --decrypt --output-type json "$TARGET_DIR/user.sops.yaml" 2>/dev/null || echo "{}")
    MERGED_JSON=$(jq -s ".[0] * .[1]" <(echo "$SHARED_JSON") <(echo "$USER_JSON"))
    KC_TEST_USERNAME=$(echo "$MERGED_JSON" | jq -r ".keycloak_test_username // empty")
    KC_TEST_PASSWORD=$(echo "$MERGED_JSON" | jq -r ".keycloak_test_password // empty")
    if [ -z "$KC_TEST_USERNAME" ] || [ -z "$KC_TEST_PASSWORD" ]; then
        echo "  ERROR: keycloak_test_username/keycloak_test_password not set in secrets.sops.yaml or user.sops.yaml" >&2
        exit 1
    fi
    USER_LIST=$(kc get users -r "$KC_REALM" -q username="$KC_TEST_USERNAME" -q exact=true 2>/dev/null || echo "[]")
    USER_EXISTS=$(echo "$USER_LIST" | jq -r ".[0].id // empty")
    if [ -z "$USER_EXISTS" ]; then
        kc create users -r "$KC_REALM" \
            -s username="$KC_TEST_USERNAME" \
            -s enabled=true \
            -s emailVerified=true \
            -s firstName=Test \
            -s lastName=User \
            -s email="$KC_TEST_USERNAME@polar.local" \
            -s "credentials=[{\"type\":\"password\",\"value\":\"$KC_TEST_PASSWORD\",\"temporary\":false}]"
        echo "  created"
    else
        echo "  already exists"
    fi

    echo "keycloak-bootstrap: done"

# Regenerate licenses/THIRD_PARTY_LICENSES.md from the agents workspace
licenses:
    #!/usr/bin/env bash
    set -euo pipefail
    manifest=src/agents/Cargo.toml
    tmp=$(mktemp -d)
    cargo metadata --no-deps --format-version 1 --manifest-path "$manifest" \
      | python3 -c 'import json,sys; [print(p["name"]) for p in json.load(sys.stdin)["packages"]]' > "$tmp/own-crates.txt"
    cargo about generate --manifest-path "$manifest" --config licenses/about.toml --format json > "$tmp/about.json"
    python3 licenses/licenses_md.py "$tmp/about.json" --exclude "$tmp/own-crates.txt" > licenses/THIRD_PARTY_LICENSES.md
    rm -rf "$tmp"

# Fail if any dependency has a license outside licenses/about.toml's allowlist
licenses-check:
    cargo about generate --manifest-path src/agents/Cargo.toml --config licenses/about.toml --format json > /dev/null

# Print nix-usernetes kubeconfig export command (dev convenience only).
cluster-ctx:
    @echo "Run: export KUBECONFIG={{_u7s_kubeconf}}"

# Load a nix result or podman image into the cluster containerd
# Usage: just cluster-load ./result-cassini-image
#        just cluster-load cassini:latest
cluster-load image:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -e "{{image}}" ]; then
        podman exec -i u7s ctr -n k8s.io images import - < "{{image}}"
    else
        podman save "{{image}}" | podman exec -i u7s ctr -n k8s.io images import -
    fi

_registry_host := "localhost"
_registry_port := "5000"

# Start a local OCI registry for image distribution. Works with any
# Kubernetes cluster (u7s, kind, k3d, a real cluster) -- push here from
# the host, and configure your cluster's node(s) to pull from wherever
# this registry is reachable on their end.
registry-up:
    podman run -d --name polar-registry -p {{_registry_port}}:{{_registry_port}} docker.io/library/registry:2 2>/dev/null || echo "Container 'polar-registry' already running."

registry-down:
    podman rm -f polar-registry

registry-status:
    curl -sf http://{{_registry_host}}:{{_registry_port}}/v2/ && echo "registry reachable"

# Pull and push the local-path provisioner image to the local registry.

