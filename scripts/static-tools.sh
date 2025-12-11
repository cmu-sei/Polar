#!/bin/bash

# Define directories

# Default to the Polar workspace; can override with --manifest-path.
MANIFEST_PATH="src/agents/Cargo.toml"

OUTPUT_DIR="./output"

skip_summary=false
show_progress=true

# NOTE: datastore_path and all output file paths are derived
# after argument parsing, once OUTPUT_DIR is final.

# Helper function to print messages in color
print_color() {
    local color_code="$1"
    local text="$2"
    echo -e "\e[${color_code}m${text}\e[0m"
}

# Function to show progress with spinning line and elapsed time
show_progress() {
    if [ "$show_progress" = true ]; then
        local msg="$1"
        local pid="$2"
        local delay=0.1
        local spinstr='\|/-'
        local start_time=$(date +%s)

        while kill -0 "$pid" 2>/dev/null; do
            local temp=${spinstr#?}
            local current_time=$(date +%s)
            local elapsed_time=$((current_time - start_time))
            printf "\r\e[33m[%c] %s (%d sec)" "$spinstr" "$msg" "$elapsed_time"
            spinstr=$temp${spinstr%"$temp"}
            sleep $delay
        done

        wait "$pid"
        print_color 32 "\r[✔] $msg ($(($(date +%s) - start_time)) sec)"
    else
        local msg="$1"
        local pid="$2"
        echo "$msg"
        wait "$pid"
    fi
}

# Function to run a command and show progress
run_with_progress() {
    local cmd="$1"
    local msg="$2"
    eval "$cmd" &
    show_progress "$msg" $!
}

# Function to display help
display_help() {
    echo "Usage: $0 [options]"
    echo "Options:"
    echo "  --skip-summary    Skip generation of the summary report"
    echo "  --no-progress     Do not display the progress indicators"
    echo "  --manifest-path   A path to a workspace or crate Cargo.toml. Looks in current directory by default."
    echo "  --output-path     A path to write output to. Defaults to current working directory."
    echo "  -h, --help        Display this help and exit"
    exit 0
}

# Function to generate summary reports
generate_summary() {
    echo "Summary Report:" > "$summary_file"
    echo "================" >> "$summary_file"
    echo "" >> "$summary_file"

    # Cargo Deny Summary
    echo "Cargo Udeps Summary:" >> "$summary_file"
    awk '/unused dependencies:/, /Note:/' "$cargo_udeps_output" | sed '1d;/Note:/Q' >> "$summary_file"
    echo "" >> "$summary_file"

    # Cargo Clippy Summary
    echo "Cargo Clippy Summary:" >> "$summary_file"
    local warning_count=$(grep -P '^\s*warning:' "$cargo_clippy_output" | grep -vP 'generated \d+ warnings|could not compile' | wc -l)
    local error_count=$(grep -P '^\s*error:' "$cargo_clippy_output" | grep -vP 'could not compile' | wc -l)
    echo "Number of warnings: $warning_count" >> "$summary_file"
    echo "Number of errors: $error_count" >> "$summary_file"
    echo "" >> "$summary_file"

    # Cargo Semver Checks Summary
    echo "Cargo Semver Checks Summary:" >> "$summary_file"
    echo "Parsed: $(grep -oP 'Parsed \[.*\]' "$cargo_semver_output" | tail -n 1)"  >> "$summary_file"
    echo "Checked: $(grep -oP 'Checked \[.*\] \d+ checks; \d+ passed, \d+ failed, \d+ unnecessary' "$cargo_semver_output")"  >> "$summary_file"
    echo "Summary: $(grep -oP 'Summary semver requires.*' "$cargo_semver_output")"  >> "$summary_file"
    echo "" >> "$summary_file"

    # Nosey Parker Scan Summary
    echo "Nosey Parker Scan Summary:" >> "$summary_file"
    awk '/Rule/,0' "$noseyparker_output" | head -n -1 >> "$summary_file"
    echo "" >> "$summary_file"

}

# Function to analyze bloat for all binaries
analyze_bloat_for_all_binaries() {
    BINARIES=( $(cargo metadata --no-deps --format-version=1 | jq -r '.packages[].targets[] | select(.kind[] | contains("bin")) | .name') )

    for BIN in "${BINARIES[@]}"
    do
        echo "Analyzing Cargo Bloat for $BIN..." >> "$cargo_bloat_all_output"
        cargo bloat --release --bin $BIN >> "$cargo_bloat_all_output" 2>&1
        echo "" >> "$cargo_bloat_all_output"
        echo "Analyzing Cargo Bloat for $BIN Crates..." >> "$cargo_bloat_all_output"
        cargo bloat --release --bin $BIN --crates >> "$cargo_bloat_all_output" 2>&1
        echo "" >> "$cargo_bloat_all_output"
    done
}

# Parse command-line options
while [ "$#" -gt 0 ]; do
    case "$1" in
        --manifest-path)
            MANIFEST_PATH="$2"
            shift 2
            ;;
        --output-path)
            if [[ -n "$2" && "$2" != --* ]]; then
                OUTPUT_DIR="$2"
                shift 2
            else
                echo "Error: --output-path requires a value."
                exit 1
            fi
            ;;
        --skip-summary)
            skip_summary=true
            shift
            ;;
        --no-progress)
            show_progress=false
            shift
            ;;
        -h|--help)
            display_help
            ;;
        *)
            echo "Unknown option: $1"
            display_help
            ;;
    esac
done

# Finalize output paths now that OUTPUT_DIR is known
mkdir -p "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR/unused-reports"

datastore_path="$OUTPUT_DIR/noseyparker_datastore"

# Define output files
cargo_deny_output="$OUTPUT_DIR/cargo_deny_output.json"
cargo_clippy_output="$OUTPUT_DIR/cargo_clippy_output.txt"
cargo_udeps_output="$OUTPUT_DIR/cargo_udeps_output.txt"
cargo_semver_output="$OUTPUT_DIR/cargo_semver_output.txt"
#cargo_unused_features_output="$OUTPUT_DIR/cargo_unused_features_output.txt"
cargo_bloat_all_output="$OUTPUT_DIR/cargo_bloat_all.txt"
noseyparker_output="$OUTPUT_DIR/noseyparker_output.txt"
noseyparker_report="$OUTPUT_DIR/noseyparker_report.txt"
summary_file="$OUTPUT_DIR/summary.txt"

# Check tools before running
required_tools=("cargo" "cargo-deny" "cargo-clippy" "cargo-udeps" "cargo-semver-checks")
for tool in "${required_tools[@]}"; do
    if ! command -v "$tool" &> /dev/null; then
        print_color "31" "Error: $tool is not installed."
        exit 1
    fi
done

# Operations
run_with_progress "cargo deny --manifest-path $MANIFEST_PATH --format json check --show-stats > $cargo_deny_output 2>&1" "Cargo Deny Check"
# run_with_progress "cargo spellcheck check > $spellcheck_output 2>&1" "Cargo Spellcheck"
run_with_progress "cargo clippy --manifest-path $MANIFEST_PATH > $cargo_clippy_output 2>&1" "Cargo Clippy"
run_with_progress "cargo udeps --manifest-path $MANIFEST_PATH > $cargo_udeps_output 2>&1" "Cargo Udeps"
run_with_progress "cargo-semver-checks semver-checks --manifest-path $MANIFEST_PATH > $cargo_semver_output 2>&1" "Cargo Semver Checks"
# cargo bloat is unmaintained, SEE: https://github.com/RazrFalcon/cargo-bloat/issues/107#issuecomment-2483438706
# run_with_progress "analyze_bloat_for_all_binaries" "Cargo Bloat Analysis"

# Get a start time for reports
START_TIME=$(date +%s)

# run_with_progress "unused-features analyze > $cargo_unused_features_output 2>&1" "Cargo Unused Features Check"

# Find JSON reports created after the start time
find /workspace/src/agents -name "report.json" -newermt "@$START_TIME" | while read file; do
    # Remove the first part up to and including the first slash
    remaining_part="${file#*/}"
    # Replace all remaining slashes in the path with underscores
    new_filename=$(echo "$remaining_part" | tr '/' '_')
    # Move the file to the output directory with the new name
    mv "$file" "$OUTPUT_DIR/unused-reports/${new_filename}_report.json"
done

# Nosey Parker Operations
rm -rf "$datastore_path"
noseyparker-cli datastore init --datastore "$datastore_path/"
run_with_progress "noseyparker-cli scan --datastore $datastore_path/ . > $noseyparker_output 2>&1" "Running Nosey Parker Scan"
noseyparker-cli report --datastore $datastore_path/ > $noseyparker_report 2>&1

# leave datastore on disk; uncomment to clean after each run
#rm -rf "$datastore_path"

# Generate Summary Report
if [ "$skip_summary" = false ]; then
    generate_summary
    print_color "32" "Reports and summaries have been compiled in the 'output' directory."
fi

# Exit with success message
print_color "32" "Execution completed successfully."
