#!/usr/bin/env nu
# static-tools.nu — Polar static analysis runner
#
# Tool coverage map:
#   cargo deny          — license compliance, banned crates, CVE advisories
#   cargo clippy        — lints across all targets and feature combinations
#   cargo udeps         — declared-but-unused dependencies
#   cargo semver-checks — API compatibility against previously published version
#   cargo geiger        — unsafe usage mapped across the dep tree
#   cargo hack          — compiles every feature combination
#   cargo mutants       — mutation testing; verifies tests actually catch bugs
#   noseyparker-cli     — secret/credential scanning across files and git history
#
# Usage:
#   nu static-tools.nu [--manifest-path <path>] [--output-path <path>]
#                      [--skip-summary] [--no-progress]
#
# The shebang above requires `nu` on PATH. In the dev container, `nu` is
# provided by the Nix environment — no additional configuration needed.

# ---------------------------------------------------------------------------
# Colour helpers
# ---------------------------------------------------------------------------

def green [msg: string] {
    $"(ansi green)($msg)(ansi reset)"
}

def red [msg: string] {
    $"(ansi red)($msg)(ansi reset)"
}

def yellow [msg: string] {
    $"(ansi yellow)($msg)(ansi reset)"
}

def bold [msg: string] {
    $"(ansi attr_bold)($msg)(ansi reset)"
}

# ---------------------------------------------------------------------------
# Preflight check
# ---------------------------------------------------------------------------

def check-tools [tools: list<string>] {
    let missing = $tools | where { |t| (which $t | is-empty) }
    if ($missing | is-not-empty) {
        print (red $"Error: missing required tools: ($missing | str join ', ')")
        exit 1
    }
}

# ---------------------------------------------------------------------------
# Core runner
# ---------------------------------------------------------------------------

# Run a single tool, capturing stdout+stderr to a file.
# Returns a result record: {name, outfile, exit_code, duration_sec}
def run-tool [
    name: string,
    outfile: string,
    cmd: list<string>,
] {
    let start = (date now)
    print (yellow $"  → ($name)...")

    let bin = $cmd.0
    let args = $cmd | skip 1

    let exit_code = do --ignore-errors {
        ^$bin ...$args out+err> $outfile
        0
    } | if ($in | is-empty) { 1 } else { $in }

    let duration = ((date now) - $start) / 1sec | math round

    let exit_code = (do --ignore-errors {
        ^$bin ...$args out+err> $outfile
        0
    } | if ($in | is-empty) { 1 } else { $in | into int })

    {name: $name, outfile: $outfile, exit_code: $exit_code, duration_sec: $duration}
}

# ---------------------------------------------------------------------------
# Tool definitions
#
# Each record describes one tool invocation. Separating data from execution
# lets us run them in parallel with par-each cleanly, and makes it trivial
# to add, remove, or reorder tools without touching control flow.
#
# NOTE: cargo-mutants is intentionally excluded from parallel execution —
# it spawns its own parallel worker processes internally and does heavy disk
# I/O; running it alongside everything else causes contention. It runs last,
# sequentially, after the parallel batch completes.
# ---------------------------------------------------------------------------

def tool-definitions [manifest: string, output_dir: string] {
    [
        {
            name: "cargo deny"
            outfile: $"($output_dir)/cargo_deny.txt"
            cmd: [
                cargo deny
                --manifest-path $manifest
                check
            ]
        }
        {
            name: "cargo clippy"
            outfile: $"($output_dir)/cargo_clippy.txt"
            cmd: [
                cargo clippy
                --manifest-path $manifest
                --all-targets
                --all-features
                --
                -D warnings
            ]
        }
        {
            name: "cargo udeps"
            outfile: $"($output_dir)/cargo_udeps.txt"
            cmd: [
                cargo udeps
                --manifest-path $manifest
                --all-targets
            ]
        }
        {
            name: "cargo semver-checks"
            outfile: $"($output_dir)/cargo_semver.txt"
            cmd: [
                cargo semver-checks
                --manifest-path $manifest
            ]
        }
        {
            name: "cargo geiger"
            outfile: $"($output_dir)/cargo_geiger.txt"
            cmd: [
                cargo geiger
                --manifest-path $manifest
            ]
        }
        {
            name: "cargo hack (feature powerset)"
            outfile: $"($output_dir)/cargo_hack.txt"
            cmd: [
                cargo hack
                check
                --manifest-path $manifest
                --feature-powerset
            ]
        }
    ]
}

# ---------------------------------------------------------------------------
# Summary generation
# ---------------------------------------------------------------------------

def count-pattern [file: string, pattern: string] {
    if (not ($file | path exists)) { return 0 }
    open $file | lines | where { |l| $l =~ $pattern } | length
}

def generate-summary [results: table, output_dir: string, skip_summary: bool] {
    if $skip_summary { return }

    let summary_file = $"($output_dir)/summary.txt"

    # Build the summary as a list of strings, then join and save.
    mut sections = [(bold "Polar Static Analysis Summary")]
    $sections = ($sections | append "")

    # Results table — structured, not grep'd
    let results_section = $results | select name exit_code duration_sec | to text
    $sections = ($sections | append (bold "=== Tool Results ==="))
    $sections = ($sections | append $results_section)
    $sections = ($sections | append "")

    # Clippy counts
    let clippy_file = $"($output_dir)/cargo_clippy.txt"
    let warnings = (count-pattern $clippy_file 'warning\[')
    let errors   = (count-pattern $clippy_file 'error\[')
    $sections = ($sections | append (bold "=== cargo clippy ==="))
    $sections = ($sections | append $"Warnings: ($warnings)  Errors: ($errors)")
    $sections = ($sections | append "")

    # cargo deny — it outputs JSON when --format json is used;
    # here we just count advisory/license lines from plain output
    let deny_file = $"($output_dir)/cargo_deny.txt"
    let deny_errors = (count-pattern $deny_file '^error')
    $sections = ($sections | append (bold "=== cargo deny ==="))
    $sections = ($sections | append $"Issues found: ($deny_errors)")
    $sections = ($sections | append "")

    # cargo udeps
    let udeps_file = $"($output_dir)/cargo_udeps.txt"
    $sections = ($sections | append (bold "=== cargo udeps ==="))
    if ($udeps_file | path exists) {
        let udeps_lines = open $udeps_file
            | lines
            | where { |l| $l =~ 'unused' or $l =~ 'crate' }
            | first 10
        if ($udeps_lines | is-not-empty) {
            $sections = ($sections | append ($udeps_lines | str join "\n"))
        } else {
            $sections = ($sections | append "No unused dependencies found.")
        }
    }
    $sections = ($sections | append "")

    # cargo semver-checks
    let semver_file = $"($output_dir)/cargo_semver.txt"
    $sections = ($sections | append (bold "=== cargo semver-checks ==="))
    if ($semver_file | path exists) {
        let semver_summary = open $semver_file
            | lines
            | where { |l| $l =~ '(Parsed|Checked|Summary|semver)' }
        $sections = ($sections | append ($semver_summary | str join "\n"))
    }
    $sections = ($sections | append "")

    # cargo geiger — last few lines are the summary table
    let geiger_file = $"($output_dir)/cargo_geiger.txt"
    $sections = ($sections | append (bold "=== cargo geiger ==="))
    if ($geiger_file | path exists) {
        let geiger_tail = open $geiger_file | lines | last 15
        $sections = ($sections | append ($geiger_tail | str join "\n"))
    }
    $sections = ($sections | append "")

    # noseyparker
    let np_report = $"($output_dir)/noseyparker_report.txt"
    $sections = ($sections | append (bold "=== noseyparker ==="))
    if ($np_report | path exists) {
        let np_lines = open $np_report | lines | first 30
        $sections = ($sections | append ($np_lines | str join "\n"))
    }
    $sections = ($sections | append "")

    # cargo mutants
    let mutants_file = $"($output_dir)/cargo_mutants.txt"
    $sections = ($sections | append (bold "=== cargo mutants ==="))
    if ($mutants_file | path exists) {
        let mutants_summary = open $mutants_file
            | lines
            | where { |l| $l =~ '(caught|missed|unviable|timeout|ok)' }
            | last 10
        $sections = ($sections | append ($mutants_summary | str join "\n"))
    }
    $sections = ($sections | append "")

    # Failed tools callout
    let failed = $results | where exit_code != 0
    if ($failed | is-not-empty) {
        $sections = ($sections | append (bold "=== Tools with non-zero exit ==="))
        let failed_names = $failed | each { |r|
            $"  ($r.name) — exit ($r.exit_code) — ($r.outfile)"
        }
        $sections = ($sections | append ($failed_names | str join "\n"))
        $sections = ($sections | append "")
    }

    $sections | str join "\n" | save --force $summary_file
    print (green $"\nSummary written to ($summary_file)")
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main [
    --manifest-path: string = "src/agents/Cargo.toml"  # Path to workspace Cargo.toml
    --output-path: string = "./output"                  # Directory for output files
    --skip-summary                                      # Skip summary report generation
    --no-progress                                       # (reserved; progress always shown for now)
] {
    let manifest  = $manifest_path
    let output_dir = $output_path

    mkdir $output_dir

    # Preflight
    check-tools [
        cargo
        cargo-deny
        cargo-udeps
        cargo-semver-checks
        cargo-geiger
        cargo-hack
        cargo-mutants
        noseyparker-cli
    ]

    print (bold "\n=== Polar Static Analysis ===\n")
    print $"Manifest : ($manifest)"
    print $"Output   : ($output_dir)"
    print ""

    # ------------------------------------------------------------------
    # Phase 1: parallel tool batch
    # ------------------------------------------------------------------
    print (bold "Phase 1 — parallel tools\n")

    let tools = (tool-definitions $manifest $output_dir)

    # par-each runs all tool closures in parallel across a thread pool.
    # Each closure captures its tool record by value, runs the command,
    # and returns a result record. The results are collected into a table
    # once all threads complete.
    let parallel_results = $tools | par-each { |tool|
        run-tool $tool.name $tool.outfile $tool.cmd
    }

    # ------------------------------------------------------------------
    # Phase 2: noseyparker (sequential — it manages its own datastore)
    # ------------------------------------------------------------------
    print ""
    print (bold "Phase 2 — noseyparker\n")

    let np_datastore = $"($output_dir)/noseyparker_datastore"
    let np_scan_out  = $"($output_dir)/noseyparker_scan.txt"
    let np_report    = $"($output_dir)/noseyparker_report.txt"

    rm -rf $np_datastore

    let np_scan_result = run-tool "noseyparker-cli scan" $np_scan_out [
        noseyparker-cli scan
        --datastore $np_datastore
        .
    ]

    # Report is generated regardless of scan exit code (0 = no findings is fine)
    noseyparker-cli report --datastore $np_datastore out+err> $np_report

    let np_report_result = {
        name: "noseyparker report"
        outfile: $np_report
        exit_code: 0   # report always exits 0; findings are in the file
        duration_sec: 0
    }

    # ------------------------------------------------------------------
    # Phase 3: cargo mutants (sequential — spawns its own workers)
    # ------------------------------------------------------------------
    print ""
    print (bold "Phase 3 — cargo mutants (this will take a while)\n")

    let mutants_out     = $"($output_dir)/cargo_mutants.txt"
    let mutants_out_dir = $"($output_dir)/mutants.out"

    let mutants_result = run-tool "cargo mutants" $mutants_out [
        cargo mutants
        --manifest-path $manifest
        --output $mutants_out_dir
    ]

    # ------------------------------------------------------------------
    # Collect all results and summarise
    # ------------------------------------------------------------------
    let all_results = (
        $parallel_results
        | append $np_scan_result
        | append $np_report_result
        | append $mutants_result
    )

    print ""
    print (bold "=== Results ===")
    print ($all_results | select name exit_code duration_sec | table)

    generate-summary $all_results $output_dir $skip_summary

    let failed = $all_results | where exit_code != 0
    if ($failed | is-not-empty) {
        print (yellow $"\n($failed | length) tools exited non-zero. Check output files in ($output_dir) for details.")
        exit 1
    } else {
        print (green "\nAll tools completed successfully.")
    }
}

