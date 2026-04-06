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

platform := `uname -m | sed 's/x86_64/x86_64-linux/;s/aarch64/aarch64-linux/'`
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
    nix build {{_nix_flags}} \
        ".#packages.{{platform}}.default" \
        ".#packages.{{platform}}.devContainer" \
        ".#packages.{{platform}}.ciContainer" \
        ".#packages.{{platform}}.agentContainer" \
        ".#packages.{{platform}}.agentContainerRocm" \
        ".#packages.{{platform}}.agentContainerVulkan" \
        ".#packages.{{platform}}.tlsCerts" \
        ".#packages.{{platform}}.cassiniImage" \
        ".#packages.{{platform}}.cassiniProducerImage" \
        ".#packages.{{platform}}.cassiniSinkImage" \
        ".#packages.{{platform}}.gitlabObserverImage" \
        ".#packages.{{platform}}.gitlabConsumerImage" \
        ".#packages.{{platform}}.kubeObserverImage" \
        ".#packages.{{platform}}.kubeConsumerImage" \
        #".#packages.{{platform}}.webObserverImage" \
        ".#packages.{{platform}}.webConsumerImage" \
        ".#packages.{{platform}}.provenanceLinkerImage" \
        ".#packages.{{platform}}.provenanceResolverImage" \
        ".#packages.{{platform}}.schedulerProcessorImage" \
        ".#packages.{{platform}}.schedulerObserverImage" \
        ".#packages.{{platform}}.jiraObserverImage" \
        ".#packages.{{platform}}.jiraConsumerImage" \
        ".#packages.{{platform}}.gitObserverImage" \
        ".#packages.{{platform}}.gitConsumerImage" \
        ".#packages.{{platform}}.gitSchedulerImage" \
        ".#packages.{{platform}}.orchestratorImage" \
        ".#packages.{{platform}}.buildProcessorImage"

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
#        just jira consumer
jira target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.jiraAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-jira-consumer
            podman load -i result-jira-observer
            podman load -i result-jira-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            podman load -i result-jira-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-jira-consumer
            podman load -i result-jira-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
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
            nix build {{_nix_flags}} "$base.consumerImage"  -o result-git-consumer
            nix build {{_nix_flags}} "$base.schedulerImage" -o result-git-scheduler
            podman load -i result-git-observer
            podman load -i result-git-consumer
            podman load -i result-git-scheduler
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-git-observer
            podman load -i result-git-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-git-consumer
            podman load -i result-git-consumer
            ;;
        scheduler)
            nix build {{_nix_flags}} "$base.schedulerImage" -o result-git-scheduler
            podman load -i result-git-scheduler
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, consumer, or scheduler." && exit 1 ;;
    esac

# ── Web (OpenAPI) agents ──────────────────────────────────────────────────────

# Build web/openapi agent images
# Usage: just web               (all)
#        just web observer
#        just web consumer
web target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.webAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-web-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-web-consumer
            podman load -i result-web-observer
            podman load -i result-web-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-web-observer
            podman load -i result-web-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-web-consumer
            podman load -i result-web-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
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
            nix build {{_nix_flags}} "$base.orchestratorImage" -o result-orchestrator-image
            nix build {{_nix_flags}} "$base.cloneImage"        -o result-clone-image
            podman load -i result-orchestrator-image
            podman load -i result-clone-image
            ;;
        agent)
            nix build {{_nix_flags}} "$base.orchestratorImage" -o result-orchestrator-image
            podman load -i result-orchestrator-image
            ;;
        clone)
            nix build {{_nix_flags}} "$base.cloneImage" -o result-clone-image
            podman load -i result-clone-image
            ;;
        *) echo "Unknown target: {{target}}. Use all, agent, or clone." && exit 1 ;;
    esac

# ── Workspace binaries ────────────────────────────────────────────────────────

# Build all workspace binaries
agents:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default" -o polar

# Build TLS certificates for local testing
tls:
    nix build {{_nix_flags}} ".#packages.{{platform}}.tlsCerts" -o result-tlsCerts
    ls -alh result-tlsCerts

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
        --ctx-size 65536 \
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

pi_version := `curl -fsSL https://api.github.com/repos/badlogic/pi-mono/releases/latest | grep '"tag_name"' | sed 's/.*"v\([^"]*\)".*/\1/'`

# Download/update pi to latest version
pi-install:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{pi_version}}"
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

_u7s_dir      := env_var('HOME') + "/Documents/projects/usernetes"
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
        podman save "$1" | podman exec -i u7s ctr -n k8s.io images import -
    }
    # Infrastructure
    if [ -n "{{neo4j_result}}" ]; then
        _load "{{neo4j_result}}"
    fi
    _load_podman docker.io/library/alpine:3.14.0
    # Polar images — all from nix result symlinks
    _load ./result-cassini-image
    _load ./result-cassini-producer-image
    _load ./result-cassini-sink-image
    _load ./result-scheduler-processor
    _load ./result-scheduler-observer
    _load ./result-orchestrator-image
    _load ./result-gitlab-observer
    _load ./result-gitlab-consumer
    _load ./result-kube-observer
    _load ./result-kube-consumer
    # Retag to match manifest image references
    _tag docker.io/library/nix-neo4j:latest                      docker.io/library/neo4j:5.26.2
    _tag docker.io/library/cassini:latest                        docker.io/library/cassini:latest
    _tag docker.io/library/harness-producer:latest               docker.io/library/harness-producer:latest
    _tag docker.io/library/harness-sink:latest                   docker.io/library/harness-sink:latest
    _tag docker.io/library/polar-scheduler-observer:latest       docker.io/library/polar-scheduler-observer:latest
    _tag docker.io/library/polar-scheduler-processor:latest      docker.io/library/polar-scheduler-processor:latest
    _tag docker.io/library/polar-gitlab-observer:latest          docker.io/library/polar-gitlab-observer:latest
    _tag docker.io/library/polar-gitlab-consumer:latest          docker.io/library/polar-gitlab-consumer:latest
    _tag docker.io/library/polar-kube-observer:latest            docker.io/library/polar-kube-observer:latest
    _tag docker.io/library/polar-kube-consumer:latest            docker.io/library/polar-kube-consumer:latest
    _tag docker.io/library/build-orchestrator:latest             docker.io/library/build-orchestrator:latest
    echo "Core images loaded and tagged."

# Render manifests for the local cluster.
# Writes values-active.dhall to select the local values file before rendering.
# See scripts/render-manifests.sh for the dhall-to-yaml invocation.
cluster-render:
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
cluster-apply:
    #!/usr/bin/env bash
    set -euo pipefail
    kc="kubectl --kubeconfig {{_u7s_kubeconf}}"
    # Namespaces, secrets, storage
    $kc apply -f manifests/polar.yaml
    $kc apply -f manifests/neo4j.yaml
    # PKI bootstrap — cert-manager resources
    $kc apply -f manifests/ca-issuer.yaml
    $kc apply -f manifests/mtls-ca.yaml
    $kc apply -f manifests/leaf-issuer.yaml
    $kc apply -f manifests/cassini-server-cert.yaml
    $kc apply -f manifests/cassini-client-certificate.yaml
    # Application workloads
    $kc apply -f manifests/cassini.yaml
    $kc apply -f manifests/jaeger.yaml
    $kc apply -f manifests/agents.yaml

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
            load ./result-web-observer
            load ./result-web-consumer
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
