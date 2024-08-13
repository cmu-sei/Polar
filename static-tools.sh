#!/bin/bash

# Define directories
src_dir="src"
#TODO: add base dir variable, and change output_dir to be relative to base
output_dir="../output" 
datastore_path="$output_dir/noseyparker_datastore"
skip_summary=false
show_progress=true

# Define output files
cargo_deny_output="$output_dir/cargo_deny_output.txt"
spellcheck_output="$output_dir/spellcheck_output.txt"
cargo_clippy_output="$output_dir/cargo_clippy_output.txt"
cargo_udeps_output="$output_dir/cargo_udeps_output.txt"
cargo_semver_output="$output_dir/cargo_semver_output.txt"
cargo_unused_features_output="$output_dir/cargo_unused_features_output.txt"
cargo_bloat_all_output="$output_dir/cargo_bloat_all.txt"
noseyparker_output="$output_dir/noseyparker_output.txt"
noseyparker_report="$output_dir/noseyparker_report.txt"
summary_file="$output_dir/summary.txt"

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

    # Cargo Unused Features Check Summary
    echo "Cargo Unused Features Check Summary:" >> "$summary_file"
    grep 'Prune' "$cargo_unused_features_output" | echo "Number of unused features: $(wc -l)" >> "$summary_file"
    echo "" >> "$summary_file"

    # Cargo Bloat Summary
    echo "Cargo Bloat Summary:" >> "$summary_file"
    local binary_count=$(grep -c 'Analyzing Cargo Bloat for' "$cargo_bloat_all_output")/2
    echo "Number of binaries scanned: $binary_count" >> "$summary_file"
    echo "" >> "$summary_file"

    # Nosey Parker Scan Summary
    echo "Nosey Parker Scan Summary:" >> "$summary_file"
    awk '/Rule/,0' "$noseyparker_output" | head -n -1 >> "$summary_file"
    echo "" >> "$summary_file"

    # Spellcheck Summary
    echo "Spellcheck Summary:" >> "$summary_file"
    local misspelled_count=$(grep -c '\^' "$spellcheck_output")
    echo "Number of misspelled words: $misspelled_count" >> "$summary_file"
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

# Setup output directory
rm -rf "$output_dir"
mkdir -p "$datastore_path" || { print_color "31" "Failed to create directory structure."; exit 1; }

# Navigate to source directory
cd "$src_dir" || { print_color "31" "Failed to change directory to $src_dir"; exit 1; }

# Check tools before running
required_tools=("cargo" "cargo-deny" "cargo-spellcheck" "cargo-clippy" "cargo-udeps" "cargo-semver-checks")
for tool in "${required_tools[@]}"; do
    if ! command -v "$tool" &> /dev/null; then
        print_color "31" "Error: $tool is not installed."
        exit 1
    fi
done

# Operations
run_with_progress "cargo deny check --show-stats > $cargo_deny_output 2>&1" "Cargo Deny Check"
run_with_progress "cargo spellcheck check > $spellcheck_output 2>&1" "Cargo Spellcheck"
run_with_progress "cargo clippy > $cargo_clippy_output 2>&1" "Cargo Clippy"
run_with_progress "cargo udeps > $cargo_udeps_output 2>&1" "Cargo Udeps"
run_with_progress "cargo-semver-checks semver-checks > $cargo_semver_output 2>&1" "Cargo Semver Checks"
run_with_progress "analyze_bloat_for_all_binaries" "Cargo Bloat Analysis"

# Create the output directory and get a start time for reports
mkdir -p $output_dir/unused-reports
START_TIME=$(date +%s)

run_with_progress "unused-features analyze > $cargo_unused_features_output 2>&1" "Cargo Unused Features Check"

# Find JSON reports created after the start time
find /workspace/src -name "report.json" -newermt "@$START_TIME" | while read file; do
    # Remove the first part up to and including the first slash
    remaining_part="${file#*/}"
    # Replace all remaining slashes in the path with underscores
    new_filename=$(echo "$remaining_part" | tr '/' '_')
    # Move the file to the output directory with the new name
    mv "$file" "$output_dir/unused-reports/${new_filename}_report.json"
done

# Nosey Parker Operations
run_with_progress "noseyparker scan --datastore $datastore_path/ . > $noseyparker_output 2>&1" "Running Nosey Parker Scan"
noseyparker report --datastore $datastore_path/ > $noseyparker_report 2>&1
rm -rf "$datastore_path"

# Generate Summary Report
if [ "$skip_summary" = false ]; then
    generate_summary
    print_color "32" "Reports and summaries have been compiled in the 'output' directory."
fi

# Exit with success message
print_color "32" "Execution completed successfully."