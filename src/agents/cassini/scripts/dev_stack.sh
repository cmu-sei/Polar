#!/usr/bin/env bash
set -euo pipefail

log() { echo "[cassini] $*"; }
die() { echo "[cassini] ERROR: $*" >&2; exit 1; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "missing command: $1"; }
need_env() { [[ -n "${!1:-}" ]] || die "required env var not set: $1 (did you 'direnv allow' at repo root?)"; }
need_file() { [[ -f "$1" ]] || die "required file missing: $1"; }
need_dir() { [[ -d "$1" ]] || die "required dir missing: $1"; }

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "${REPO_ROOT}" ]] || die "not in a git repo (need Polar checkout)"
CASSINI_ROOT="${REPO_ROOT}/src/agents/cassini"
[[ -d "${CASSINI_ROOT}" ]] || die "cassini root not found at ${CASSINI_ROOT}"

# Workspace target dir (your actual build output location)
AGENTS_ROOT="${REPO_ROOT}/src/agents"
WORKSPACE_TARGET_DIR="${AGENTS_ROOT}/target/debug"

RUNS_ROOT="${CASSINI_ROOT}/output/dev/runs"
mkdir -p "${RUNS_ROOT}"

CURRENT_LINK="${CASSINI_ROOT}/output/dev/current"

# These will be set per-run by begin_run()
RUN_ID=""
RUN_DIR=""
PID_DIR=""
LOG_DIR=""

CTRL_PID=""
CTRL_LOG=""

CTRL_MANIFEST="${CASSINI_ROOT}/test/control/Cargo.toml"
[[ -f "${CTRL_MANIFEST}" ]] || die "missing control manifest: ${CTRL_MANIFEST}"

# Prefer native by default; allow compose if you add a cassini-local compose later
COMPOSE_FILE=""
for f in \
  "${CASSINI_ROOT}/docker-compose.yml" \
  "${CASSINI_ROOT}/compose.yml" \
  "${CASSINI_ROOT}/test/docker-compose.yml" \
  "${CASSINI_ROOT}/broker/docker-compose.yml"
do
  [[ -f "$f" ]] && { COMPOSE_FILE="$f"; break; }
done

MODE="${MODE:-auto}"
if [[ "${MODE}" == "auto" ]]; then
  [[ -n "${COMPOSE_FILE}" ]] && MODE="compose" || MODE="native"
fi

# ---- TLS env must already exist (Phase 1 shellHook)
need_env TLS_CA_CERT
need_env TLS_SERVER_CERT_CHAIN
need_env TLS_SERVER_KEY
need_env TLS_CLIENT_CERT
need_env TLS_CLIENT_KEY
need_file "${TLS_CA_CERT}"
need_file "${TLS_SERVER_CERT_CHAIN}"
need_file "${TLS_SERVER_KEY}"
need_file "${TLS_CLIENT_CERT}"
need_file "${TLS_CLIENT_KEY}"

# Defaults (don’t clobber user overrides)
: "${BROKER_ADDR:=127.0.0.1:8080}"
: "${CONTROLLER_BIND_ADDR:=127.0.0.1:3030}"
: "${CONTROLLER_ADDR:=${CONTROLLER_BIND_ADDR}}"
: "${CASSINI_SERVER_NAME:=localhost}"
: "${CONTROLLER_SERVER_NAME:=${CASSINI_SERVER_NAME}}"
: "${RUST_LOG:=info}"

# Control plane config (per README)
: "${CONFIG_PATH:=${CASSINI_ROOT}/test/control/config.dhall}"
need_file "${CONFIG_PATH}"

host_of() { echo "$1" | awk -F: '{print $1}'; }
port_of() { echo "$1" | awk -F: '{print $2}'; }

# mTLS probe: supply client cert/key. Without these, your broker will (correctly) complain.
mtls_probe() {
  local host="$1" port="$2" sni="$3"
  need_cmd openssl

  log "mTLS probe (verify + client cert + SNI=${sni}) to ${host}:${port} ..."
  # We send a real TLS client hello + present a client cert.
  # We also verify server cert against our CA.
  timeout 10 openssl s_client \
    -connect "${host}:${port}" \
    -servername "${sni}" \
    -CAfile "${TLS_CA_CERT}" \
    -cert "${TLS_CLIENT_CERT}" \
    -key  "${TLS_CLIENT_KEY}" \
    -verify_return_error \
    </dev/null >/dev/null 2>&1
}

is_pid_running() {
  local pidfile="$1"
  [[ -f "$pidfile" ]] || return 1
  local pid; pid="$(cat "$pidfile" 2>/dev/null || true)"
  [[ -n "$pid" ]] || return 1
  kill -0 "$pid" >/dev/null 2>&1
}

stop_pidfile() {
  local pidfile="$1" name="$2"
  [[ -f "$pidfile" ]] || return 0
  local pid; pid="$(cat "$pidfile" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
    log "stopping ${name} (pid ${pid})"
    kill "$pid" >/dev/null 2>&1 || true
    sleep 0.5
    kill -0 "$pid" >/dev/null 2>&1 && kill -9 "$pid" >/dev/null 2>&1 || true
  fi
  rm -f "$pidfile"
}

compose_up() {
  [[ -n "${COMPOSE_FILE}" ]] || die "MODE=compose but no compose file found under ${CASSINI_ROOT}"
  need_cmd docker
  if docker compose version >/dev/null 2>&1; then
    DC=(docker compose)
  else
    need_cmd docker-compose
    DC=(docker-compose)
  fi
  log "compose up: ${COMPOSE_FILE}"
  "${DC[@]}" -f "${COMPOSE_FILE}" -p cassini up -d
}

compose_down() {
  [[ -n "${COMPOSE_FILE}" ]] || return 0
  if docker compose version >/dev/null 2>&1; then
    DC=(docker compose)
  else
    DC=(docker-compose)
  fi
  log "compose down: ${COMPOSE_FILE}"
  "${DC[@]}" -f "${COMPOSE_FILE}" -p cassini down || true
}

# --- Key point: DO NOT start broker directly here.
# The control plane spawns:
#   ./target/debug/cassini-server
#   ./target/debug/harness-producer
#   ./target/debug/harness-sink
# but in this repo those binaries live in src/agents/target/debug.
#
# We solve this in two layers:
#   1) Build the bins into the workspace target dir.
#   2) Provide a compatibility symlink tree at cassini/target/debug -> workspace bins,
#      so the control plane's hardcoded ./target/debug/... paths work.
#
# Returns 0 if python3 exists; we use it to parse cargo metadata without jq.
need_python() { command -v python3 >/dev/null 2>&1 || die "missing command: python3 (needed to parse cargo metadata)"; }

build_binaries() {
  need_cmd cargo
  need_python
  need_dir "${AGENTS_ROOT}"

  # These are the binaries the control plane tries to spawn at:
  #   ./target/debug/cassini-server
  #   ./target/debug/harness-producer
  #   ./target/debug/harness-sink
  local wanted_bins=(cassini-server harness-producer harness-sink)

  log "discovering cassini harness binaries via cargo metadata..."
  # We run metadata from the workspace root so the build output goes to:
  #   ${AGENTS_ROOT}/target/debug
  local meta

  # Build only the bins we care about, by resolving (package, bin) pairs.
  # This avoids guessing package names (which already bit us).
  local pairs
pairs="$(
  cd "${AGENTS_ROOT}" \
  && cargo metadata --format-version 1 --no-deps \
  | python3 -c '
import json, sys
meta = json.load(sys.stdin)
wanted = sys.argv[1:]
wanted_set = set(wanted)

out = []
for pkg in meta.get("packages", []):
    for tgt in pkg.get("targets", []):
        if "bin" not in set(tgt.get("kind", [])):
            continue
        name = tgt.get("name")
        if name in wanted_set:
            out.append((name, pkg.get("name")))

order = {b:i for i,b in enumerate(wanted)}
out.sort(key=lambda x: order.get(x[0], 9999))
for bin_name, pkg_name in out:
    print(f"{bin_name}\t{pkg_name}")
' -- "${wanted_bins[@]}"
)"

  # Sanity: ensure we found all bins.
  local found=0
  while IFS=$'\t' read -r bin pkg; do
    [[ -n "${bin}" && -n "${pkg}" ]] || continue
    found=$((found+1))
  done <<< "${pairs}"

  if [[ "${found}" -ne "${#wanted_bins[@]}" ]]; then
    log "metadata result (bin -> package):"
    echo "${pairs}" | sed 's/^/[cassini]   /' || true
    die "could not resolve all required bins via cargo metadata. Wanted: ${wanted_bins[*]}"
  fi

  log "building harness binaries into workspace target dir..."
  while IFS=$'\t' read -r bin pkg; do
    [[ -n "${bin}" && -n "${pkg}" ]] || continue
    log "  cargo build -p ${pkg} --bin ${bin}"
    ( cd "${AGENTS_ROOT}" && cargo build -q -p "${pkg}" --bin "${bin}" ) || die "cargo build failed for ${pkg} (${bin})"
  done <<< "${pairs}"

  # Final check: ensure the binaries exist where we expect them (workspace target dir)
  for b in "${wanted_bins[@]}"; do
    [[ -x "${WORKSPACE_TARGET_DIR}/${b}" ]] || die "expected binary missing after build: ${WORKSPACE_TARGET_DIR}/${b}"
  done
}

ensure_target_symlinks() {
  # Control plane expects ./target/debug/* relative to CASSINI_ROOT
  local compat_target="${CASSINI_ROOT}/target/debug"
  mkdir -p "${compat_target}"
  need_dir "${WORKSPACE_TARGET_DIR}"

  # IMPORTANT: names here must match actual produced binaries.
  # If any differ, you'll see a clear error below.
  local bins=(cassini-server harness-producer harness-sink)

  for b in "${bins[@]}"; do
    if [[ ! -x "${WORKSPACE_TARGET_DIR}/${b}" ]]; then
      die "expected binary missing (or not executable): ${WORKSPACE_TARGET_DIR}/${b} (check Cargo [[bin]] name)"
    fi
    ln -sf "${WORKSPACE_TARGET_DIR}/${b}" "${compat_target}/${b}"
  done
}

native_up() {
  need_cmd cargo
  : > "${CTRL_LOG}"

  # Canonical: broker binds to BIND_ADDR (env consumed by BrokerArgs::new()).
  : "${BIND_ADDR:=${BROKER_ADDR:-127.0.0.1:8080}}"
  : "${BROKER_ADDR:=${BIND_ADDR}}"

  export BIND_ADDR BROKER_ADDR CONTROLLER_BIND_ADDR CONTROLLER_ADDR CASSINI_SERVER_NAME RUST_LOG
  export TLS_CA_CERT TLS_SERVER_CERT_CHAIN TLS_SERVER_KEY TLS_CLIENT_CERT TLS_CLIENT_KEY

  build_binaries
  ensure_target_symlinks

  log "starting control plane (native) ..."
  (
    cd "${CASSINI_ROOT}"
    exec cargo run --quiet --manifest-path "${CTRL_MANIFEST}" -- \
      --config "${CONFIG_PATH}" \
      2>&1 | tee -a "${CTRL_LOG}"
  ) &
  echo $! > "${CTRL_PID}"
}

native_down() {
  stop_pidfile "${CTRL_PID}" "control"
  # The control plane spawns broker/producer/sink.
  # If it doesn't reap children on exit, you'll see ports stuck in use.
  # Kill anything still holding the expected ports as a cleanup safety valve.
  if command -v lsof >/dev/null 2>&1; then
    local bp cp
    bp="$(port_of "${BROKER_ADDR}")"
    cp="$(port_of "${CONTROLLER_BIND_ADDR}")"
    for p in "${bp}" "${cp}"; do
      local pids
      pids="$(lsof -t -iTCP:"${p}" -sTCP:LISTEN 2>/dev/null || true)"
      if [[ -n "${pids}" ]]; then
        log "cleanup: killing listeners on port ${p}: ${pids}"
        kill ${pids} >/dev/null 2>&1 || true
      fi
    done
  fi
}

status() {
  log "mode=${MODE}"
  if [[ "${MODE}" == "compose" ]]; then
    # unchanged...
    :
  else
    if [[ -L "${CURRENT_LINK}" ]]; then
      log "current_run=$(readlink "${CURRENT_LINK}")"
      local cur_pid="${CURRENT_LINK}/pids/control.pid"
      local cur_log="${CURRENT_LINK}/logs/control.log"
      if is_pid_running "${cur_pid}"; then
        log "control: RUNNING (pid $(cat "${cur_pid}"))"
      else
        log "control: STOPPED"
      fi
      log "logs:"
      log "  ${cur_log}"
    else
      log "no current run (have you run: $0 up ?)"
    fi
  fi
}

logs() {
  [[ "${MODE}" == "native" ]] || die "logs is for native mode (compose has docker logs)"
  [[ -L "${CURRENT_LINK}" ]] || die "no current run symlink at ${CURRENT_LINK} (run: $0 up)"
  tail -n 200 -f "${CURRENT_LINK}/logs/control.log"
}

wait_mtls() {
  local host="$1" port="$2" sni="$3" label="$4"
  log "waiting for ${label} mTLS at ${host}:${port} (SNI=${sni}) ..."
  local i
  for i in $(seq 1 60); do
    if mtls_probe "${host}" "${port}" "${sni}"; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

wait_port() {
  local host="$1" port="$2" label="$3"
  log "waiting for ${label} at ${host}:${port} ..."
  timeout 30 bash -c "until (echo >/dev/tcp/${host}/${port}) >/dev/null 2>&1; do sleep 0.2; done" \
    || die "${label} did not open ${host}:${port} within 30s"
}

begin_run() {
  # RFC3339-ish, sortable, filesystem-safe
  local ts rand
  ts="$(date -u +'%Y%m%dT%H%M%SZ')"
  rand="$(tr -dc 'a-z0-9' </dev/urandom | head -c 6 || true)"
  RUN_ID="${ts}-${rand}"

  RUN_DIR="${RUNS_ROOT}/${RUN_ID}"
  PID_DIR="${RUN_DIR}/pids"
  LOG_DIR="${RUN_DIR}/logs"
  mkdir -p "${PID_DIR}" "${LOG_DIR}"

  CTRL_PID="${PID_DIR}/control.pid"
  CTRL_LOG="${LOG_DIR}/control.log"

  ln -sfn "${RUN_DIR}" "${CURRENT_LINK}"

  log "run_id=${RUN_ID}"
  log "run_dir=${RUN_DIR}"
}

up() {
  begin_run

  log "bringing up cassini harness stack (mode=${MODE})"
  log "  BROKER_ADDR=${BROKER_ADDR}"
  log "  CONTROLLER_BIND_ADDR=${CONTROLLER_BIND_ADDR}"
  log "  CONTROLLER_ADDR=${CONTROLLER_ADDR}"
  log "  CASSINI_SERVER_NAME=${CASSINI_SERVER_NAME}"
  log "  CONFIG_PATH=${CONFIG_PATH}"
  log "  WORKSPACE_TARGET_DIR=${WORKSPACE_TARGET_DIR}"

  if [[ "${MODE}" == "compose" ]]; then
    compose_up
  else
    if is_pid_running "${CTRL_PID}"; then
      die "already running (try: $0 status or $0 restart)"
    fi
    native_up
  fi

  # Readiness checks:
  # - Controller: just wait for the port to accept a TCP connect.
  #   Do NOT TLS-probe it; openssl vs rustls can create flaky aborts.
  # - Broker: verify it’s accepting mTLS (client cert + CA verify + SNI).
  local ch cp bh bp
  ch="$(host_of "${CONTROLLER_BIND_ADDR}")"; cp="$(port_of "${CONTROLLER_BIND_ADDR}")"
  bh="$(host_of "${BROKER_ADDR}")";          bp="$(port_of "${BROKER_ADDR}")"

  wait_port "${ch}" "${cp}" "control plane"

  # Broker is spawned by the controller; verify it’s accepting mTLS.
  if ! wait_mtls "${bh}" "${bp}" "${CASSINI_SERVER_NAME}" "broker"; then
    if [[ "${STRICT_READY:-0}" == "1" ]]; then
      die "broker mTLS probe failed (CA/SNI/cert/key mismatch?) host=${bh} port=${bp} sni=${CASSINI_SERVER_NAME}"
    else
      log "WARN: broker mTLS probe failed, but stack may still be healthy (continuing)."
    fi
  fi

  log "ready."
}

down() {
  log "bringing down cassini harness stack (mode=${MODE})"
  if [[ "${MODE}" == "compose" ]]; then
    compose_down
  else
    native_down
  fi
  log "stopped."
}

restart() { down; up; }

cmd="${1:-}"
case "${cmd}" in
  up) up ;;
  down) down ;;
  restart) restart ;;
  status) status ;;
  logs) logs ;;
  *)
    cat <<EOF
Usage: $0 {up|down|restart|status|logs}

Env:
  MODE=auto|native|compose
  CONFIG_PATH=.../test/control/config.dhall
  BROKER_ADDR=127.0.0.1:8080
  CONTROLLER_BIND_ADDR=127.0.0.1:3030
  CONTROLLER_ADDR=127.0.0.1:3030
  CASSINI_SERVER_NAME=localhost
  RUST_LOG=info

Notes:
  - In native mode, this script does NOT start broker/producer/sink directly.
    The control plane spawns them and expects binaries under ./target/debug/.
    We build to src/agents/target/debug and symlink into src/agents/cassini/target/debug.
EOF
    exit 2
    ;;
esac
