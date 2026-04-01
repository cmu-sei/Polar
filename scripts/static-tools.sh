#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Polar static analysis runner
# ---------------------------------------------------------------------------
# Tool mapping (what each tool is for):
#   cargo deny        – license compliance, banned crates, CVE advisories
#   cargo clippy      – lints (correctness, perf, style) across all targets/features
#   cargo udeps       – finds declared-but-unused dependencies
#   cargo semver-checks – checks API compatibility against previous published version
#   cargo geiger      – maps unsafe usage across dep tree
#   cargo hack        – compiles every feature combination; catches feature-gated breakage
#   cargo mutants     – mutation testing; verifies tests actually catch bugs
#   noseyparker-cli   – secret/credential scanning across files and git history
# ---------------------------------------------------------------------------

MANIFEST_PATH="src/agents/Cargo.toml"
OUTPUT_DIR="./output"
skip_summary=false
show_progress=true

print_color() {
    local color_code="$1"
    local text="$2"
    echo -e "\e[${color_code}m${text}\e[0m"
}

show_progress_spinner() {
    if [ "$show_progress" = false ]; then
        return
    fi
    local msg="$1"
    local pid="$2"
    local spinstr='\|/-'
    local start_time
    start_time=$(date +%s)

    while kill -0 "$pid" 2>/dev/null; do
        local temp=${spinstr#?}
        local elapsed=$(( $(date +%s) - start_time ))
        printf "\r\e[33m[%c] %s (%d sec)\e[0m" "$spinstr" "$msg" "$elapsed"
        spinstr=$temp${spinstr%"$temp"}
        sleep 0.1
    done

    local exit_code=0
    wait "$pid" || exit_code=$?
    local elapsed=$(( $(date +%s) - start_time ))

    if [ "$exit_code" -eq 0 ]; then
        print_color 32 "\r[✔] $msg (${elapsed}s)          "
    else
        print_color 31 "\r[✗] $msg failed (exit $exit_code, ${elapsed}s)    "
        # Don't abort — collect all results, report failures in summary.
        FAILED_TOOLS+=("$msg")
    fi
}

# Run a command in the background with a spinner. Non-zero exit is recorded
# but does not abort the whole script (tools like clippy exit 1 on warnings).
run_tool() {
    local msg="$1"
    local outfile="$2"
    shift 2
    # "$@" is the command — no eval, no injection risk.
    "$@" > "$outfile" 2>&1 &
    show_progress_spinner "$msg" $!
}

display_help() {
    cat <<EOF
Usage: $0 [options]
Options:
  --manifest-path PATH  Path to workspace or crate Cargo.toml (default: $MANIFEST_PATH)
  --output-path PATH    Directory to write output files (default: $OUTPUT_DIR)
  --skip-summary        Skip generation of the summary report
  --no-progress         Disable progress spinners
  -h, --help            Show this help and exit
EOF
    exit 0
}

generate_summary() {
    {
        echo "Summary Report"
        echo "=============="
        echo ""

        echo "### Cargo Clippy"
        local warnings errors
        warnings=$(grep -cP '^\s*warning:' "$cargo_clippy_output" || true)
        errors=$(grep -cP '^\s*error:' "$cargo_clippy_output" || true)
        echo "Warnings: $warnings  Errors: $errors"
        echo ""

        echo "### Cargo Udeps"
        grep -A999 'unused dependencies:' "$cargo_udeps_output" | head -30 || echo "(none found)"
        echo ""

        echo "### Cargo Semver Checks"
        grep -oP '(Parsed|Checked|Summary semver).*' "$cargo_semver_output" || echo "(no output)"
        echo ""

        echo "### Cargo Geiger"
        tail -20 "$cargo_geiger_output" || echo "(no output)"
        echo ""

        echo "### Nosey Parker"
        grep -A999 'Rule' "$noseyparker_report" | head -40 || echo "(no findings)"
        echo ""

        echo "### Cargo Deny"
        grep -E '(error|warning)\[' "$cargo_deny_output" | head -20 || echo "(no issues)"
        echo ""

        if [ "${#FAILED_TOOLS[@]}" -gt 0 ]; then
            echo "### Tools that exited non-zero"
            for t in "${FAILED_TOOLS[@]}"; do
                echo "  - $t"
            done
        fi
    } > "$summary_file"
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
FAILED_TOOLS=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --manifest-path) MANIFEST_PATH="$2"; shift 2 ;;
        --output-path)
            [[ -n "$2" && "$2" != --* ]] || { echo "Error: --output-path requires a value."; exit 1; }
            OUTPUT_DIR="$2"; shift 2 ;;
        --skip-summary) skip_summary=true; shift ;;
        --no-progress) show_progress=false; shift ;;
        -h|--help) display_help ;;
        *) echo "Unknown option: $1"; display_help ;;
    esac
done

# ---------------------------------------------------------------------------
# Output paths
# ---------------------------------------------------------------------------
mkdir -p "$OUTPUT_DIR"
datastore_path="$OUTPUT_DIR/noseyparker_datastore"

cargo_deny_output="$OUTPUT_DIR/cargo_deny.txt"
cargo_clippy_output="$OUTPUT_DIR/cargo_clippy.txt"
cargo_udeps_output="$OUTPUT_DIR/cargo_udeps.txt"
cargo_semver_output="$OUTPUT_DIR/cargo_semver.txt"
cargo_geiger_output="$OUTPUT_DIR/cargo_geiger.txt"
cargo_hack_output="$OUTPUT_DIR/cargo_hack.txt"
cargo_mutants_output="$OUTPUT_DIR/cargo_mutants.txt"
noseyparker_output="$OUTPUT_DIR/noseyparker_scan.txt"
noseyparker_report="$OUTPUT_DIR/noseyparker_report.txt"
summary_file="$OUTPUT_DIR/summary.txt"

# ---------------------------------------------------------------------------
# Preflight: check all required tools are on PATH
# ---------------------------------------------------------------------------
required_tools=(
    cargo
    cargo-deny
    cargo-udeps
    cargo-semver-checks
    cargo-geiger
    cargo-hack
    cargo-mutants
    noseyparker-cli
)
missing=()
for tool in "${required_tools[@]}"; do
    command -v "$tool" &>/dev/null || missing+=("$tool")
done
if [ "${#missing[@]}" -gt 0 ]; then
    print_color 31 "Error: missing tools: ${missing[*]}"
    exit 1
fi

# ---------------------------------------------------------------------------
# Analysis runs
# ---------------------------------------------------------------------------

# cargo deny: license compliance, banned crates, CVE advisories
run_tool "cargo deny" "$cargo_deny_output" \
    cargo deny --manifest-path "$MANIFEST_PATH" check

# cargo clippy: lints across ALL targets and feature combinations
run_tool "cargo clippy" "$cargo_clippy_output" \
    cargo clippy --manifest-path "$MANIFEST_PATH" \
        --all-targets --all-features -- -D warnings

# cargo udeps: find declared-but-unused dependencies (requires nightly)
run_tool "cargo udeps" "$cargo_udeps_output" \
    cargo udeps --manifest-path "$MANIFEST_PATH" --all-targets

# cargo semver-checks: API compatibility against previously published version
run_tool "cargo semver-checks" "$cargo_semver_output" \
    cargo semver-checks --manifest-path "$MANIFEST_PATH"

# cargo geiger: map unsafe usage across the dep tree
run_tool "cargo geiger" "$cargo_geiger_output" \
    cargo geiger --manifest-path "$MANIFEST_PATH"

# cargo hack: compile every feature combination (catches feature-gated breakage)
run_tool "cargo hack (feature powerset check)" "$cargo_hack_output" \
    cargo hack check --manifest-path "$MANIFEST_PATH" --feature-powerset

# cargo mutants: mutation testing (slow; consider --in-diff for CI)
# Tip: add --in-diff <patch-file> in CI to only mutate lines changed in a PR.
run_tool "cargo mutants" "$cargo_mutants_output" \
    cargo mutants --manifest-path "$MANIFEST_PATH" --output "$OUTPUT_DIR/mutants.out"

# ---------------------------------------------------------------------------
# Nosey Parker: secret scanning
# ---------------------------------------------------------------------------
rm -rf "$datastore_path"
# Scan then report in two steps so the report is always generated even if
# scan finds nothing (noseyparker exits 0 regardless of findings).
noseyparker-cli scan --datastore "$datastore_path" . > "$noseyparker_output" 2>&1
noseyparker-cli report --datastore "$datastore_path" > "$noseyparker_report" 2>&1
print_color 32 "[✔] noseyparker"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
if [ "$skip_summary" = false ]; then
    generate_summary
fi

if [ "${#FAILED_TOOLS[@]}" -gt 0 ]; then
    print_color 33 "Completed with non-zero exits from: ${FAILED_TOOLS[*]}"
    print_color 33 "Check individual output files in $OUTPUT_DIR for details."
else
    print_color 32 "All tools completed successfully. Output in $OUTPUT_DIR"
fi
