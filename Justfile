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

# The target whose registry the image and registry recipes use: its own, in the cluster, through a forward they start
# (infra/cluster-registry.nu). POLAR_TARGET, else local-minimal; `just target=sandbox-local image cassini` for another.
# `just target= images-push ...` pushes to POLAR_REGISTRY instead (default localhost:5000, such as `just registry-host-up`'s).
target := env_var_or_default('POLAR_TARGET', 'local-minimal')
_target_flag := if target == "" { "" } else { "--target " + target }

# ── Help ──────────────────────────────────────────────────────────────────────

# List all available recipes
default:
    @just --list

# ── Helpers (private) ─────────────────────────────────────────────────────────

# Load a docker-archive build result into the host podman.
# No-ops when podman is unavailable (e.g. inside the dev container, where
# podman-in-podman is explicitly not wanted and images reach the cluster through
# its registry instead: `just image <name>`), or when POLAR_SKIP_LOAD is set. This lets the
# per-service build recipes run unchanged in both the dev container and on the
# host — in the container they build only; on the host they also load.
_load result:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${POLAR_SKIP_LOAD:-}" ]; then
        echo "POLAR_SKIP_LOAD set — skipping load of {{result}}"
        exit 0
    fi
    if ! command -v {{_runtime}} >/dev/null 2>&1; then
        echo "{{_runtime}} not found — skipping load of {{result}} (build-only)"
        exit 0
    fi
    {{_runtime}} load -i "{{result}}"

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

# Use this to verify the full build after disruptive changes. The table's images are built by infra/images.nu (the
# images-build recipe, first); the rest are what the table does not hold: the workspace binaries and the CI and agent containers.
# Build every image in infra/images.nuon, then the binaries and the containers it does not list
build-all: images-build
    nix build {{_nix_flags}} ".#packages.{{platform}}.default"              -o result-binaries
    nix build {{_nix_flags}} ".#packages.{{platform}}.ciContainer"          -o result-ci-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainer"       -o result-agent-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerRocm"   -o result-agent-container-rocm
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerVulkan" -o result-agent-container-vulkan

# ── Dev container ─────────────────────────────────────────────────────────────

# Build and load the dev container
dev-container:
    echo "Building dev container ({{platform}})..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer" -o result-dev-container
    just _load result-dev-container

# ── Workspace binaries ────────────────────────────────────────────────────────

# Build all workspace binaries
agents:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default" -o polar

# Pin the Dhall side of nix-container-lib (src/containers/container-lib.dhall) to the commit flake.lock names for its Nix side,
# with the hash Dhall computes for it, then re-render every container config that imports it. Each rendered .nix opens with the
# license banner of the .dhall it comes from, as src/flake/Justfile's renders do. Review with git diff.
# Pin container-lib.dhall to flake.lock's nix-container-lib and re-render the container configs
update-dhall-pins:
    #!/usr/bin/env bash
    set -euo pipefail
    COMMIT=$(nix flake metadata --json | jq -r '.locks.nodes["nix-container-lib"].locked.rev')
    URL="https://raw.githubusercontent.com/daveman1010221/nix-container-lib/${COMMIT}/dhall/prelude.dhall"
    echo "+ dhall hash <<< $URL"
    HASH=$(dhall hash <<< "$URL")
    echo "  nix-container-lib $COMMIT, Dhall prelude $HASH"
    LIB=src/containers/container-lib.dhall
    echo "+ pinning $LIB"
    sed -i -E "s|https://raw\.githubusercontent\.com/daveman1010221/nix-container-lib/[0-9a-f]{40}/dhall/prelude\.dhall|$URL|; s|^([[:space:]]*)sha256:[0-9a-f]{64}$|\1$HASH|" "$LIB"
    for f in $(grep -rl "container-lib.dhall" src/ --include="*.dhall" | grep -v '^src/containers/container-lib.dhall$'); do
        DIR="$(dirname "$f")"; BASE="$(basename "$f" .dhall)"
        echo "+ (cd $DIR && dhall-to-nix < $BASE.dhall > $BASE.nix)"
        # Rendered aside and then written into the existing file, which keeps its mode; a failed render leaves it as it was.
        ( cd "$DIR" && { sed -n '/^--/!q; s/^--/#/p' "$BASE.dhall"; dhall-to-nix < "$BASE.dhall"; } > "$BASE.nix.new" && cat "$BASE.nix.new" > "$BASE.nix" && rm "$BASE.nix.new" ) \
            || { rm -f "$DIR/$BASE.nix.new"; echo "  FAILED: $f" >&2; exit 1; }
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
    nu scripts/pipeline/static-tools.nu --manifest-path src/agents/Cargo.toml

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
# Usage: just user-cert-install            (defaults to the current user)
#        just user-cert-install alice
# POLAR_NSS_DB=<profile dir> overrides which profile is used (for testing on a copy).
user-cert-install username=env_var('USER'):
    bash infra/user-cert-install.sh {{username}}

# ── polar-admin-grant wrappers ───────────────────────────────────────────────
#
# The observer URL is the port-forward from `just port-forwards-up`. Override
# for another cluster:  POLAR_OBSERVER_URL=https://... just admin-claim ...
# (product code has no default; this is only the dev convenience.)

_admin_dir    := env_var('HOME') + "/.config/polar-admin"
# The container runtime: POLAR_CONTAINER_RUNTIME, else podman or docker, whichever is installed (Polar provides neither).
_runtime      := env_var_or_default('POLAR_CONTAINER_RUNTIME', `if command -v podman >/dev/null 2>&1; then echo podman; elif command -v docker >/dev/null 2>&1; then echo docker; else echo podman; fi`)
# The admin tool: polar-admin-grant on PATH (see `nu infra/tools.nu install`), else built from this checkout.
_admin_grant  := `if command -v polar-admin-grant >/dev/null 2>&1; then echo polar-admin-grant; else echo "cargo run -q --manifest-path $(pwd)/src/agents/Cargo.toml -p polar-admin-grant --"; fi`
_observer_url := env_var_or_default('POLAR_OBSERVER_URL', 'http://localhost:8081')

# Step 1: fetch the CA, then redeem the one-shot bootstrap credential.
# Writes ~/.config/polar-admin/{ca.pem,bootstrap-admin.crt,bootstrap-admin.key}.
admin-bootstrap:
    mkdir -p {{_admin_dir}}
    chmod 700 {{_admin_dir}}
    kubectl -n polar get configmap polar-ca -o jsonpath='{.data.ca\.pem}' > {{_admin_dir}}/ca.pem
    {{_admin_grant}} \
        --observer-url {{_observer_url}} bootstrap \
        --cert-out {{_admin_dir}}/bootstrap-admin.crt \
        --key-out  {{_admin_dir}}/bootstrap-admin.key

# Step 2: provision a normal admin account, signed with the bootstrap cert.
# The account can provision accounts, grant permissions and revoke certificates (c:revoke), and sign the two kinds of
# document the agents act on: a configuration (c:update-configuration) and a command (c:run-command). Capabilities
# are fixed when the certificate is issued, so an account provisioned before one was added to this
# list does not have it; provision another, or reissue.
# The bootstrap certificate is spent by the first provisioning, so a later account is provisioned
# by an account that already holds c:provision-account: name it as the signer.
# Usage: just admin-provision alice alice@example.org
#        just admin-provision operator ops@example.org alice
admin-provision username email signer="bootstrap-admin":
    {{_admin_grant}} \
        --cert {{_admin_dir}}/{{signer}}.crt \
        --key  {{_admin_dir}}/{{signer}}.key \
        --observer-url {{_observer_url}} provision-account \
        --username {{username}} --email {{email}} \
        --permission p:u2s:polar-db-svc.polar-graph.svc.cluster.local \
        --permission p:u2s:credentials-agent-observer.polar.svc.cluster.local \
        --capability c:provision-account --capability c:grant \
        --capability c:update-configuration --capability c:run-command \
        --capability c:update-policy --capability c:update-runtime \
        --capability c:update-groups --capability c:revoke \
        --reason user-onboarding

# Step 3: claim the provisioned account. Generates the keypair locally (the
# key never leaves this machine) and writes ~/.config/polar-admin/<username>.{crt,key}.
# Usage: just admin-claim alice alice@example.org
admin-claim username email:
    {{_admin_grant}} \
        --observer-url {{_observer_url}} claim \
        --username {{username}} --email {{email}} \
        --cert-out {{_admin_dir}}/{{username}}.crt \
        --key-out  {{_admin_dir}}/{{username}}.key

# Stops Neo4j (suspending the Flux Kustomization that applies it, on a gitops target), loads the dump with Neo4j's own image
# and user, and starts it again; the users and the password are kept (infra/neo4j-load.nu says how).
#   just neo4j-load-dump ~/Documents/projects/jira.5.26.26.neo4j.dump
# Replace the cluster graph's neo4j database with a dump
neo4j-load-dump dump_path:
    nu infra/neo4j-load.nu {{dump_path}}

# The image recipes are infra/images.nu (its header says what each stage needs), and print the command they run. Those that
# push use the target's registry (`target`, above): the one in its cluster, through a forward they start.

# Build the nix images (archives under result/). Needs only nix: no registry, no cluster.
# With no names, builds every image.  just images-build kube-observer
images-build *names:
    nu infra/images.nu build {{ if names == "" { "--all" } else { names } }}

# Build and push to the target's registry. With no names, every image.
images-push *names:
    nu infra/images.nu push {{ if names == "" { "--all" } else { names } }} {{_target_flag}}

# Copy third-party images from upstream into the target's registry. With no names, every one.
images-mirror *names:
    nu infra/images.nu mirror {{ if names == "" { "--all" } else { names } }} {{_target_flag}}

# Every stage for the named images: build, push, mirror.
#   just image kube-observer jira-observer
image +names:
    nu infra/images.nu sync {{names}} {{_target_flag}}

# What `cluster-load` did (import an image into the node), for any cluster: build it and push it to the cluster's registry.
alias cluster-load := image

# Every stage for every image in infra/images.nuon.
images-all:
    nu infra/images.nu sync --all {{_target_flag}}

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
#
# `registry` (a target whose registry is in the cluster, infra/cluster-registry.nu)
# starts only when named, since it takes local 5000, where the host registry
# listens: POLAR_REGISTRY_FORWARD_PORT picks another port. `deploy` (a gitops
# target's deployment git server, infra/deploy-repo.nu) starts only when named
# too, on local 9443 or POLAR_DEPLOY_FORWARD_PORT.
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
    if [[ "$ONLY" == *" registry "* ]]; then
        start_pf registry polar-registry registry "${POLAR_REGISTRY_FORWARD_PORT:-5000}:5000" /tmp/pf-registry.log
    fi
    if [[ "$ONLY" == *" deploy "* ]]; then
        start_pf deploy polar-deploy polar-deploy-git "${POLAR_DEPLOY_FORWARD_PORT:-9443}:9443" /tmp/pf-deploy.log
    fi
    echo "Logs: /tmp/pf-*.log"

# The polar-git-server, as the git CLI reaches it from this machine: the cluster's own name for it,
# which is the name on its certificate, mapped to the port-forward (just port-forwards-up git).
# git checks the certificate's name against the URL's host, so the URL keeps the cluster name and
# curl is told where that name lives (http.curloptResolve, git 2.37 or newer).
_git_host := "polar-git-server.polar.svc.cluster.local"

# The git configuration that makes `git` present your Polar certificate to the Polar git server.
# Prints it; changes nothing. GIT_SSL_CAINFO in the environment overrides http.sslCAInfo, so unset
# it (in fish: set -e GIT_SSL_CAINFO) or use `just polar-git-clone`, which does it for one command.
# Usage: just polar-git-config            (as the current user)
#        just polar-git-config alice
polar-git-config user=env_var('USER'):
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
polar-git-clone repo dest="" user=env_var('USER'):
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
# Usage: just cassini-shutdown            (as the current user)
#        just cassini-shutdown alice
cassini-shutdown user=env_var('USER'):
    nu infra/cassini-shutdown.nu --user {{user}}

# What the secret harness pods say about the credentials agent's secret policy, and whether it held
# (src/agents/secret-harness). The owner needs two rounds (a minute apart) and the intruder starts after
# 90 seconds, so run it a few minutes after the pods are up; it exits non-zero until the policy has held.
secret-harness-results:
    nu infra/harness.nu

# Run a load-harness plan on the cluster (src/agents/cassini/test; docs/architecture/policy-enforcement.md, stage 5a):
# each run's sinks, then its producers, as Jobs; the records go to ~/.cache/polar/harness. The target must enable
# harness-producer and harness-sink (its config.nuon and target.nu say how).
#        just load-harness-run infra/load-plans/baseline.nuon
load-harness-run plan target='local-minimal':
    nu infra/load-harness.nu run {{plan}} --target {{target}}

# The runs in load-harness records, side by side.
#        just load-harness-compare ~/.cache/polar/harness/baseline-*.nuon
load-harness-compare +records:
    nu infra/load-harness.nu compare {{records}}

# Stop all detached port-forwards started by port-forwards-up.
port-forwards-down:
    #!/usr/bin/env bash
    pkill -f "port-forward -n polar-graph svc/polar-db-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar-auth svc/keycloak-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/ace-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/jaeger-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/credentials-agent-observer" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/polar-git-server" 2>/dev/null || true
    pkill -f "port-forward -n polar-registry svc/registry" 2>/dev/null || true
    pkill -f "port-forward -n polar-deploy svc/polar-deploy-git" 2>/dev/null || true
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

_registry_host := "localhost"
_registry_port := "5000"

# Applied with the storage it needs, and forwarded to this machine, which pushes through the forward; up.nu's registry step.
# Start the target's registry in its cluster (infra/cluster-registry.nu)
registry-up:
    nu infra/cluster-registry.nu up {{target}}

# Whether the target's registry in the cluster runs, and its forward answers.
registry-status:
    nu infra/cluster-registry.nu status {{target}}

# For a target that names a registry you bring (no `registry_in_cluster`): a container on localhost:5000, with podman or
# docker. Push to it with `just target= images-push ...`; your nodes must be able to reach it.
# Start a registry on this machine
registry-host-up:
    {{_runtime}} run -d --name polar-registry -p {{_registry_port}}:{{_registry_port}} docker.io/library/registry:2 2>/dev/null || echo "Container 'polar-registry' already running."

# Stop the registry on this machine
registry-host-down:
    {{_runtime}} rm -f polar-registry

# Whether the registry on this machine answers
registry-host-status:
    curl -sf http://{{_registry_host}}:{{_registry_port}}/v2/ && echo "registry reachable"
