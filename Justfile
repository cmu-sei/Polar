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
    podman load -i result-agent-container

# Build every package — binaries, all containers, TLS certs.
# Use this to verify the full build after disruptive changes.
build-all:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default"               -o result-binaries
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer"          -o result-dev-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.ciContainer"           -o result-ci-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainer"        -o result-agent-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerRocm"    -o result-agent-container-rocm
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerVulkan"  -o result-agent-container-vulkan
    nix build {{_nix_flags}} ".#packages.{{platform}}.tlsCerts"              -o result-tls-certs
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniImage"          -o result-cassini
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniProducerImage"  -o result-harness-producer
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniSinkImage"      -o result-harness-sink
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitlabObserverImage"   -o result-gitlab-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitlabConsumerImage"   -o result-gitlab-consumer
    nix build {{_nix_flags}} ".#packages.{{platform}}.kubeObserverImage"     -o result-kube-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.kubeConsumerImage"     -o result-kube-consumer
    nix build {{_nix_flags}} ".#packages.{{platform}}.openApiObserverImage"  -o result-openapi-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.openApiProcessorImage" -o result-openapi-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.provenanceLinkerImage" -o result-provenance-linker
    nix build {{_nix_flags}} ".#packages.{{platform}}.provenanceResolverImage" -o result-provenance-resolver
    nix build {{_nix_flags}} ".#packages.{{platform}}.schedulerProcessorImage" -o result-scheduler-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.schedulerObserverImage"  -o result-scheduler-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.jiraObserverImage"     -o result-jira-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.jiraProcessorImage"    -o result-jira-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitObserverImage"      -o result-git-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitProcessorImage"     -o result-git-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitSchedulerImage"     -o result-git-scheduler
    nix build {{_nix_flags}} ".#packages.{{platform}}.orchestratorImage"     -o result-build-orchestrator
    nix build {{_nix_flags}} ".#packages.{{platform}}.buildProcessorImage"   -o result-build-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.nuInitImage"           -o result-nu-init
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitServerImage"        -o result-git-server

# ── Dev container ─────────────────────────────────────────────────────────────

# Build and load the dev container
dev-container:
    echo "Building dev container ({{platform}})..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer" -o result-dev-container
    podman load -i result-dev-container

# ── Cassini ───────────────────────────────────────────────────────────────────

# Build cassini images
# Usage: just cassini           (all images)
#        just cassini server
#        just cassini producer
#        just cassini sink
#        just cassini client
cassini target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.cassini"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.cassiniImage"  -o result-cassini-image
            nix build {{_nix_flags}} "$base.producerImage" -o result-cassini-producer-image
            nix build {{_nix_flags}} "$base.sinkImage"     -o result-cassini-sink-image
            podman load -i result-cassini-image
            podman load -i result-cassini-producer-image
            podman load -i result-cassini-sink-image
            ;;
        server)
            nix build {{_nix_flags}} "$base.cassiniImage" -o result-cassini-image
            podman load -i result-cassini-image
            ;;
        producer)
            nix build {{_nix_flags}} "$base.producerImage" -o result-cassini-producer-image
            podman load -i result-cassini-producer-image
            ;;
        sink)
            nix build {{_nix_flags}} "$base.sinkImage" -o result-cassini-sink-image
            podman load -i result-cassini-sink-image
            ;;
        client)
            nix build {{_nix_flags}} "$base.clientBin" -o result-cassini-client
            ;;
        *) echo "Unknown target: {{target}}. Use all, server, producer, sink, or client." && exit 1 ;;
    esac

# ── GitLab agents ─────────────────────────────────────────────────────────────

# Build gitlab agent images
# Usage: just gitlab            (all)
#        just gitlab observer
#        just gitlab consumer
gitlab target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.gitlabAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-gitlab-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-gitlab-consumer
            podman load -i result-gitlab-observer
            podman load -i result-gitlab-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-gitlab-observer
            podman load -i result-gitlab-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-gitlab-consumer
            podman load -i result-gitlab-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
    esac

# ── Kubernetes agents ─────────────────────────────────────────────────────────

# Build kubernetes agent images
# Usage: just kube              (all)
#        just kube observer
#        just kube consumer
kube target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.kubeAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-kube-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-kube-consumer
            podman load -i result-kube-observer
            podman load -i result-kube-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-kube-observer
            podman load -i result-kube-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-kube-consumer
            podman load -i result-kube-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
    esac

# ── Jira agents ───────────────────────────────────────────────────────────────
# Build jira agent images
# Usage: just jira              (all)
#        just jira observer
#        just jira processor
jira target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.jiraAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            nix build {{_nix_flags}} "$base.processorImage" -o result-jira-processor
            podman load -i result-jira-observer
            podman load -i result-jira-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            podman load -i result-jira-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-jira-processor
            podman load -i result-jira-processor
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or processor." && exit 1 ;;
    esac

# ── Git agents ────────────────────────────────────────────────────────────────

# Build git agent images
# Usage: just git-agents        (all)
#        just git-agents observer
#        just git-agents consumer
#        just git-agents scheduler
git-agents target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.gitAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage"  -o result-git-observer
            nix build {{_nix_flags}} "$base.processorImage" -o result-git-processor
            nix build {{_nix_flags}} "$base.schedulerImage" -o result-git-scheduler
            podman load -i result-git-observer
            podman load -i result-git-processor
            podman load -i result-git-scheduler
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-git-observer
            podman load -i result-git-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-git-processor
            podman load -i result-git-processor
            ;;
        scheduler)
            nix build {{_nix_flags}} "$base.schedulerImage" -o result-git-scheduler
            podman load -i result-git-scheduler
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, processor, or scheduler." && exit 1 ;;
    esac

# ── OpenAPI agents ────────────────────────────────────────────────────────────

# Build openapi agent images
# Usage: just openapi               (all)
#        just openapi observer
#        just openapi processor
openapi target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.webAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage"  -o result-openapi-observer
            nix build {{_nix_flags}} "$base.processorImage" -o result-openapi-processor
            podman load -i result-openapi-observer
            podman load -i result-openapi-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-openapi-observer
            podman load -i result-openapi-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-openapi-processor
            podman load -i result-openapi-processor
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or processor." && exit 1 ;;
    esac

# ── Provenance agents ─────────────────────────────────────────────────────────

# Build provenance agent images
# Usage: just provenance        (all)
#        just provenance linker
#        just provenance resolver
provenance target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.provenance"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.linkerImage"   -o result-provenance-linker
            nix build {{_nix_flags}} "$base.resolverImage" -o result-provenance-resolver
            podman load -i result-provenance-linker
            podman load -i result-provenance-resolver
            ;;
        linker)
            nix build {{_nix_flags}} "$base.linkerImage" -o result-provenance-linker
            podman load -i result-provenance-linker
            ;;
        resolver)
            nix build {{_nix_flags}} "$base.resolverImage" -o result-provenance-resolver
            podman load -i result-provenance-resolver
            ;;
        *) echo "Unknown target: {{target}}. Use all, linker, or resolver." && exit 1 ;;
    esac

# ── Scheduler agents ──────────────────────────────────────────────────────────

# Build scheduler agent images
# Usage: just scheduler         (all)
#        just scheduler processor
#        just scheduler observer
scheduler target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.scheduler"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.processorImage" -o result-scheduler-processor
            nix build {{_nix_flags}} "$base.observerImage"  -o result-scheduler-observer
            podman load -i result-scheduler-processor
            podman load -i result-scheduler-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-scheduler-processor
            podman load -i result-scheduler-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-scheduler-observer
            podman load -i result-scheduler-observer
            ;;
        *) echo "Unknown target: {{target}}. Use all, processor, or observer." && exit 1 ;;
    esac

# ── Build orchestrator ────────────────────────────────────────────────────────

# Build orchestrator images
# Usage: just orchestrator          (all)
#        just orchestrator agent
#        just orchestrator clone
orchestrator target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.buildOrchestrator"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.orchestratorImage"  -o result-build-orchestrator
            nix build {{_nix_flags}} "$base.cloneImage"         -o result-clone-image
            nix build {{_nix_flags}} "$base.buildProcessorImage" -o result-build-processor
            podman load -i result-build-orchestrator
            podman load -i result-clone-image
            podman load -i result-build-processor
            ;;
        agent)
            nix build {{_nix_flags}} "$base.orchestratorImage" -o result-build-orchestrator
            podman load -i result-build-orchestrator
            ;;
        clone)
            nix build {{_nix_flags}} "$base.cloneImage" -o result-clone-image
            podman load -i result-clone-image
            ;;
        processor)
                    nix build {{_nix_flags}} "$base.buildProcessorImage" -o result-build-processor
                    podman load -i result-build-processor
                    ;;
                *) echo "Unknown target: {{target}}. Use all, agent, clone, or processor." && exit 1 ;;
            esac

# ── Workspace binaries ────────────────────────────────────────────────────────

# Build all workspace binaries
agents:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default" -o polar

# Build TLS certificates for local testing
tls:
    nix build {{_nix_flags}} ".#packages.{{platform}}.tlsCerts" -o result-tls-certs
    ls -alh result-tls-certs

# Build the nu-init infrastructure container
nu-init:
    echo "Building nu-init container..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.nuInitImage" -o result-nu-init
    podman load -i result-nu-init

git-server:
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitServerImage" -o result-git-server-image
    podman load -i result-git-server-image

# Update the nix-container-lib pin in container-lib.dhall and re-render all container.nix files.
update-dhall-pins:
    #!/usr/bin/env bash
    set -euo pipefail
    COMMIT=$(nix flake metadata --json | jq -r '.locks.nodes["nix-container-lib"].locked.rev')
    DHALL_HASH=$(dhall hash <<< "https://raw.githubusercontent.com/daveman1010221/nix-container-lib/${COMMIT}/dhall/prelude.dhall")
    echo "nix-container-lib commit: $COMMIT"
    echo "Dhall prelude hash:       $DHALL_HASH"
    echo "Updating src/containers/container-lib.dhall..."
    sed -i "s|/nix-container-lib/[a-f0-9]\{40\}/dhall/prelude.dhall|/nix-container-lib/${COMMIT}/dhall/prelude.dhall|g" src/containers/container-lib.dhall
    sed -i "s|sha256:[a-f0-9]\{64\}|${DHALL_HASH}|g" src/containers/container-lib.dhall
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

# Start the local docker-compose stack
run-compose:
    podman compose -f src/conf/docker-compose.yml up -d

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
        --hf-file omnicoder-9b-q4_k_m.gguf \
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
        -ctk q4_0 -ctv q4_0 > /tmp/llama-server.log 2>&1

llm-omnicoder-nvidia-12GB:
    llama-server \
        --hf-repo Tesslate/OmniCoder-9B-GGUF \
        --hf-file omnicoder-9b-q4_k_m.gguf \
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
        -ctk q4_0 -ctv q4_0 > /tmp/llama-server.log 2>&1

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

# ── nix-usernetes (local cluster) ────────────────────────────────────────────
#
# Full cluster startup sequence:
#
#   cd ~/Documents/projects/nix-usernetes
#   just load-node-image   # after every nix build
#   just reset && just up && just init && just kubeconfig
#   export KUBECONFIG=$(pwd)/kubeconfig
#
#   cd ~/Documents/projects/Polar
#   just cluster-install-cert-manager
#   just cluster-render && just cluster-apply-storage && just cluster-load-all && just cluster-apply

_u7s_dir      := env_var('HOME') + "/Documents/projects/nix-usernetes"
_u7s_kubeconf := _u7s_dir + "/kubeconfig"

# Start the local cluster
cluster-up:
    just -f {{_u7s_dir}}/Justfile up

# Stop the local cluster
cluster-down:
    just -f {{_u7s_dir}}/Justfile down

# Initialize the cluster (first time only)
cluster-init:
    just -f {{_u7s_dir}}/Justfile init

# Install cert-manager into the cluster. Required before deploying Polar.
# cert-manager is a Polar concern — not permanent cluster infrastructure.
cluster-install-cert-manager:
    just -f {{_u7s_dir}}/Justfile install-cert-manager

# Wipe all cluster state
cluster-reset:
    just -f {{_u7s_dir}}/Justfile reset

# Show cluster status
cluster-status:
    KUBECONFIG={{_u7s_kubeconf}} kubectl get nodes
    KUBECONFIG={{_u7s_kubeconf}} kubectl get pods -A

# Open a shell inside the cluster node
cluster-shell:
    podman exec -it u7s bash

# Switch kubectl context to local cluster
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

_neo4j_result := env_var('HOME') + "/Documents/projects/nix-neo4j/result-neo4j-image"

# Build all images needed for cluster deployment with correct result symlink names
cluster-build-all:
    just cassini all
    just gitlab all
    just kube all
    just scheduler all
    just orchestrator all
    just provenance all
    just git-agents all
    just jira all
    just openapi all
    just nu-init

# Build and load all core Polar images into the cluster.
# All images are loaded directly from nix result symlinks.
# Usage: just cluster-load-all
#        just cluster-load-all neo4j_result=../nix-neo4j/result-neo4j-image
cluster-load-all neo4j_result=_neo4j_result:
    #!/usr/bin/env bash
    set -euo pipefail
    _load() {
        if [ -e "$1" ]; then
            echo "Loading $1..."
            podman exec -i u7s ctr -n k8s.io images import - < "$1"
        else
            echo "Skipping $1 (not found)"
        fi
    }
    _tag() {
        echo "Tagging $1 -> $2"
        podman exec u7s ctr -n k8s.io images tag "$1" "$2" 2>/dev/null || true
    }
    _load_podman() {
        echo "Loading $1 from podman..."
        if ! podman image exists "$1" 2>/dev/null; then
            echo "Pulling $1..."
            podman pull "$1"
        fi
        podman save "$1" | podman exec -i u7s ctr -n k8s.io images import -
    }
    # Infrastructure
    if [ -n "{{neo4j_result}}" ]; then
        _load "{{neo4j_result}}"
    fi
    # Polar images — all from nix result symlinks
    _load ./result-cassini
    _load ./result-harness-producer
    _load ./result-harness-sink
    _load ./result-clone-image
    _load ./result-gitlab-observer
    _load ./result-gitlab-consumer
    _load ./result-kube-observer
    _load ./result-kube-consumer
    _load ./result-git-observer
    _load ./result-git-processor
    _load ./result-git-scheduler
    _load ./result-jira-observer
    _load ./result-jira-processor
    _load ./result-openapi-observer
    _load ./result-openapi-processor
    _load ./result-scheduler-observer
    _load ./result-scheduler-processor
    _load ./result-provenance-linker
    _load ./result-provenance-resolver
    _load ./result-build-orchestrator
    _load ./result-build-processor
    _load ./result-nu-init
    # Retag to match manifest image references
    _tag docker.io/library/cassini:latest                       docker.io/library/cassini:latest
    _tag docker.io/library/harness-producer:latest              docker.io/library/harness-producer:latest
    _tag docker.io/library/harness-sink:latest                  docker.io/library/harness-sink:latest
    _tag docker.io/library/cyclops/git-clone:latest             docker.io/library/cyclops/git-clone:latest
    _tag docker.io/library/gitlab-observer:latest               docker.io/library/gitlab-observer:latest
    _tag docker.io/library/gitlab-consumer:latest               docker.io/library/gitlab-consumer:latest
    _tag docker.io/library/kube-observer:latest                 docker.io/library/kube-observer:latest
    _tag docker.io/library/kube-consumer:latest                 docker.io/library/kube-consumer:latest
    _tag docker.io/library/git-observer:latest                  docker.io/library/git-observer:latest
    _tag docker.io/library/git-processor:latest                 docker.io/library/git-processor:latest
    _tag docker.io/library/git-scheduler:latest                 docker.io/library/git-scheduler:latest
    _tag docker.io/library/jira-observer:latest                 docker.io/library/jira-observer:latest
    _tag docker.io/library/jira-processor:latest                docker.io/library/jira-processor:latest
    _tag docker.io/library/openapi-observer:latest              docker.io/library/openapi-observer:latest
    _tag docker.io/library/openapi-processor:latest             docker.io/library/openapi-processor:latest
    _tag docker.io/library/scheduler-observer:latest            docker.io/library/scheduler-observer:latest
    _tag docker.io/library/scheduler-processor:latest           docker.io/library/scheduler-processor:latest
    _tag docker.io/library/provenance-linker:latest             docker.io/library/provenance-linker:latest
    _tag docker.io/library/provenance-resolver:latest           docker.io/library/provenance-resolver:latest
    _tag docker.io/library/build-orchestrator:latest            docker.io/library/build-orchestrator:latest
    _tag docker.io/library/build-processor:latest               docker.io/library/build-processor:latest
    _tag docker.io/library/nix-neo4j:latest                     docker.io/library/neo4j:5.26.2
    _tag docker.io/library/polar-nu-init:latest                 docker.io/library/polar-nu-init:latest
    echo "Core images loaded and tagged."

# Render manifests for the local cluster.
# Writes values-active.dhall to select the local values file before rendering.
# See scripts/render-manifests.sh for the dhall-to-yaml invocation.
cluster-render:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f src/deploy/local/conf/git.json ]; then
        echo "ERROR: src/deploy/local/conf/git.json missing. Copy from git.json.example and fill in credentials."
        exit 1
    fi
    for var in NEO4J_TLS_CA_CERT_CONTENT NEO4J_TLS_SERVER_CERT_CONTENT NEO4J_TLS_SERVER_KEY_CONTENT; do
        if [ -z "${!var:-}" ]; then
            echo "ERROR: $var is not set. Source example.env and fill in the TLS values."
            exit 1
        fi
    done
    echo './values.local.dhall' > src/deploy/local/values-active.dhall
    bash scripts/render-manifests.sh src/deploy/local manifests

# Pull and load the local-path provisioner image into the cluster
cluster-load-provisioner:
    #!/usr/bin/env bash
    set -euo pipefail
    img="docker.io/rancher/local-path-provisioner:v0.0.35"
    if ! podman image exists "$img" 2>/dev/null; then
        echo "Pulling $img..."
        podman pull "$img"
    fi
    echo "Loading $img into cluster..."
    podman save "$img" | podman exec -i u7s ctr -n k8s.io images import -
    echo "Done."

# Apply storage infrastructure (local-path provisioner + managed-csi StorageClass)
cluster-apply-storage: cluster-load-provisioner
    #!/usr/bin/env bash
    set -euo pipefail
    KUBECONFIG={{_u7s_kubeconf}} kubectl apply -f src/deploy/local/pki/local-path-provisioner.yaml
    KUBECONFIG={{_u7s_kubeconf}} kubectl apply -f manifests/storage.yaml

# Apply all rendered Polar manifests to the local cluster.
# Order matters: namespaces and storage must exist before dependent resources.
# Apply all rendered Polar manifests to the local cluster.
cluster-apply:
    nu infra/render.nu local --apply

# ── kind (local cluster) ──────────────────────────────────────────────────────

_kind_cluster  := "polar"
_kind_config   := "src/conf/kind-cluster.yaml"
_deploy_local  := "src/deploy/local"
_manifests_out := "manifests-local"

# Create the local kind cluster (idempotent)
kind-up:
    #!/usr/bin/env bash
    set -euo pipefail
    if kind get clusters 2>/dev/null | grep -qx "{{_kind_cluster}}"; then
        echo "Cluster '{{_kind_cluster}}' already exists — skipping."
    else
        echo "Creating kind cluster '{{_kind_cluster}}'..."
        kind create cluster --name {{_kind_cluster}} --config {{_kind_config}}
    fi
    kubectl cluster-info --context kind-{{_kind_cluster}}

# Tear down the local kind cluster
kind-down:
    #!/usr/bin/env bash
    set -euo pipefail
    if kind get clusters 2>/dev/null | grep -qx "{{_kind_cluster}}"; then
        kind delete cluster --name {{_kind_cluster}}
    else
        echo "No cluster '{{_kind_cluster}}' — nothing to do."
    fi

# Switch kubectl context to the local kind cluster
kind-ctx:
    kubectl config use-context kind-{{_kind_cluster}}

# Show cluster health at a glance
kind-status:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "=== kind clusters ==="
    kind get clusters
    echo ""
    echo "=== nodes ==="
    kubectl get nodes --context kind-{{_kind_cluster}}
    echo ""
    echo "=== pods (all namespaces) ==="
    kubectl get pods -A --context kind-{{_kind_cluster}}

# Load an image into kind — accepts either a nix result path or a podman image name.
# Usage: just kind-load ./result-cassini-image
#        just kind-load cassini:latest
kind-load image:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -e "{{image}}" ]; then
        echo "Loading nix result '{{image}}' into kind cluster '{{_kind_cluster}}'..."
        kind load image-archive {{image}} --name {{_kind_cluster}}
    else
        echo "Loading podman image '{{image}}' into kind cluster '{{_kind_cluster}}'..."
        podman save {{image}} | kind load image-archive /dev/stdin --name {{_kind_cluster}}
    fi

# Build and load all core Polar images into kind (skips podman entirely).
# Requires: kind-up, and nix-neo4j result-neo4j-image available at NEO4J_IMAGE_PATH.
# Usage: just kind-load-all
#        just NEO4J_IMAGE_PATH=~/projects/nix-neo4j/result-neo4j-image kind-load-all
kind-load-all neo4j_result='':
    #!/usr/bin/env bash
    set -euo pipefail

    load() { just kind-load "$1"; }

    # Infrastructure
    if [ -n "{{neo4j_result}}" ]; then
        load "{{neo4j_result}}"
    else
        echo "Skipping neo4j — pass neo4j_result=<path> to include it."
        echo "  e.g. just kind-load-all neo4j_result=../nix-neo4j/result-neo4j-image"
    fi

    # Cassini
    just cassini all
    load ./result-cassini-image
    load ./result-cassini-producer-image
    load ./result-cassini-sink-image

    # Scheduler
    just scheduler all
    load ./result-scheduler-processor
    load ./result-scheduler-observer

    # Build orchestrator
    just orchestrator all
    load ./result-orchestrator-image
    load ./result-build-processor-image
    load ./result-clone-image

    echo "Core images loaded. Agent images (gitlab, kube, git, web, provenance) load on demand:"
    echo "  just kind-load-agents gitlab"
    echo "  just kind-load-agents kube"
    echo "  etc."

# Build and load a specific agent pair (or all agents) into kind.
# Usage: just kind-load-agents gitlab
#        just kind-load-agents kube
#        just kind-load-agents git
#        just kind-load-agents web
#        just kind-load-agents provenance
#        just kind-load-agents all
kind-load-agents agent='all':
    #!/usr/bin/env bash
    set -euo pipefail
    load() { just kind-load "$1"; }
    case "{{agent}}" in
        gitlab)
            just gitlab all
            load ./result-gitlab-observer
            load ./result-gitlab-consumer
            ;;
        kube)
            just kube all
            load ./result-kube-observer
            load ./result-kube-consumer
            ;;
        git)
            just git-agents all
            load ./result-git-observer
            load ./result-git-consumer
            load ./result-git-scheduler
            ;;
        web)
            just web all
            load ./result-openapi-observer
            load ./result-openapi-processor
            ;;
        provenance)
            just provenance all
            load ./result-provenance-linker
            load ./result-provenance-resolver
            ;;
        all)
            just kind-load-agents gitlab
            just kind-load-agents kube
            just kind-load-agents git
            just kind-load-agents web
            just kind-load-agents provenance
            ;;
        *) echo "Unknown agent: {{agent}}. Use gitlab, kube, git, web, provenance, or all." && exit 1 ;;
    esac

# Render Dhall manifests for local kind deployment.
# Output goes to _manifests_out (gitignored, rebuilt on demand).
kind-render:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Rendering manifests from {{_deploy_local}} -> {{_manifests_out}}..."
    rm -rf {{_manifests_out}}
    bash scripts/render-manifests.sh {{_deploy_local}} {{_manifests_out}}
    echo "Rendered manifests in {{_manifests_out}}/"

# Apply rendered manifests to the local kind cluster.
kind-apply:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -d "{{_manifests_out}}" ]; then
        echo "No manifests found — run 'just kind-render' first."
        exit 1
    fi
    kubectl apply -R -f {{_manifests_out}} --context kind-{{_kind_cluster}}

# Render and apply in one step.
kind-deploy: kind-render kind-apply

# Port-forward core services to localhost (runs in foreground — use a separate pane).
# Jaeger UI, Neo4j browser, and Cassini are forwarded.
# Note: kind's extraPortMappings already handle these for NodePort services;
# this target is for ClusterIP services or when you want explicit control.
kind-forward:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Forwarding services (Ctrl-C to stop all)..."
    kubectl port-forward -n polar     svc/cassini-ip-svc  8080:8080 --context kind-{{_kind_cluster}} &
    kubectl port-forward -n polar     svc/jaeger-svc     16686:16686 4317:4317 4318:4318 --context kind-{{_kind_cluster}} &
    kubectl port-forward -n polar-db  svc/polar-db-svc    7474:7474 7687:7687 --context kind-{{_kind_cluster}} &
    wait
