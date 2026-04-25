
#!/usr/bin/env nu
# ci.nu — unified Polar build pipeline
#
# Runs the full build sequence under a single Cassini daemon:
#
#   Phase 0 — Static analysis (no Cassini, no artifacts)
#     Parallel: cargo deny, clippy, udeps, semver-checks, geiger, hack
#     Sequential: noseyparker (manages its own datastore), cargo mutants
#     (spawns its own workers). Failures are reported; --strict-analysis
#     aborts the pipeline on any non-zero exit.
#
#   Phase 1 — Cargo SBOMs
#     cargo-cyclonedx generates a CycloneDX SBOM per workspace member,
#     each is parsed into a graph fragment and emitted as sbom.resolved.
#
#   Phase 2 — Cargo binaries
#     cargo build --message-format=json produces ELF artifacts; each binary
#     is linked back to its Phase 1 SBOM via a cryptographic binding digest.
#     sbom_lookup flows in-process from Phase 1 — no file-based IPC needed.
#
#   Phase 3 — OCI images
#     nix build → syft SBOM → skopeo upload → cosign sign, one pass per
#     image in the manifest. Source-level SBOM hashes from Phase 1 are
#     threaded into image.linked events, closing the full provenance chain:
#       (OCIArtifact)-[:BUILT_FROM]->(Package)<-[:DESCRIBES]-(SourceSbom)
#
# All phases share one artifact directory. Phases 1-3 share one Cassini daemon.
# Adding a new image = one row in image-manifest. Nothing else changes.

use core.nu *

const COMPONENT           = "ci"
const ELF_BINARY_ARTIFACT = "elf-binary"

# ===========================================================================
# Container bootstrap
#
# When ci.nu is used as the container entrypoint instead of start.nu, it
# must reproduce the subset of start.nu that CI actually depends on.
#
# DESIGN: we deliberately avoid hardcoding /nix/store/... paths. Store hashes
# change every time the container image is rebuilt from a new nixpkgs pin, so
# any hardcoded path is a maintenance time-bomb. Instead we discover paths at
# runtime through two stable indirections:
#
#   1. The Nix profile at /nix/var/nix/profiles/default
#      The container-lib installs all packages into this profile, which the Nix
#      tooling maintains as a symlink tree. Paths like:
#        /nix/var/nix/profiles/default/lib/pkgconfig
#        /nix/var/nix/profiles/default/include
#        /nix/var/nix/profiles/default/lib
#      are stable across rebuilds regardless of what hash is in the store.
#
#   2. pkg-config, once PKG_CONFIG_PATH is set from the profile
#      pkg-config --variable=prefix openssl gives us the openssl store path
#      without us having to know it in advance. Same for libclang via llvm.
#
# Everything else in start.nu (user creation, Fish shell, Dropbear, banner,
# fish plugins, sftp symlink, exec chroot) is interactive/dev scaffolding
# that has no place in a CI run.
# ===========================================================================

# Resolve a pkg-config variable for a package, returning "" on failure.
# PKG_CONFIG_PATH must already be set before calling this.
def pc-var [pkg: string, var: string]: nothing -> string {
    try { ^pkg-config --variable $var $pkg | str trim } catch { "" }
}

def bootstrap-container-env [] {
    let profile = "/nix/var/nix/profiles/default"

    # ── PKG_CONFIG_PATH ──────────────────────────────────────────────────────
    # Set this first — everything else that uses pkg-config depends on it.
    # The profile lib/pkgconfig directory is the canonical aggregation point
    # for all .pc files installed into the profile, regardless of store hash.
    let pc_dir = $"($profile)/lib/pkgconfig"
    let existing_pc = ($env.PKG_CONFIG_PATH? | default "")
    $env.PKG_CONFIG_PATH = if ($existing_pc | is-empty) {
        $pc_dir
    } else {
        $"($pc_dir):($existing_pc)"
    }

    # ── OpenSSL ──────────────────────────────────────────────────────────────
    # openssl-sys checks OPENSSL_DIR, OPENSSL_LIB_DIR, OPENSSL_INCLUDE_DIR in
    # that order before falling back to pkg-config. We resolve them from
    # pkg-config so the values track whatever openssl version is in the profile.
    let openssl_prefix  = (pc-var "openssl" "prefix")
    let openssl_libdir  = (pc-var "openssl" "libdir")

    if ($openssl_prefix | is-not-empty) {
        $env.OPENSSL_DIR         = $openssl_prefix
        $env.OPENSSL_LIB_DIR     = $openssl_libdir
        $env.OPENSSL_INCLUDE_DIR = $"($openssl_prefix)/include"
    } else {
        # pkg-config found nothing — fall back to profile symlink paths.
        # These exist as long as openssl-dev is in the profile even if the
        # .pc file is absent or malformed.
        log-warn "pkg-config could not resolve openssl — falling back to profile paths" --component $COMPONENT
        $env.OPENSSL_DIR         = $profile
        $env.OPENSSL_LIB_DIR     = $"($profile)/lib"
        $env.OPENSSL_INCLUDE_DIR = $"($profile)/include"
    }

    # ── LIBCLANG_PATH ────────────────────────────────────────────────────────
    # bindgen needs the directory containing libclang.so. llvm-config gives us
    # the authoritative answer; we fall back to the profile lib dir if clang
    # is not on PATH or llvm-config is absent.
    let libclang = (
        try { ^llvm-config --libdir | str trim } catch {
            try {
                # llvm-config not on PATH as such — try resolving from clang binary location
                let clang_bin = (^which clang | str trim)
                ^($clang_bin | path dirname | path join ".." "lib") | path expand | into string
            } catch {
                $"($profile)/lib"
            }
        }
    )
    $env.LIBCLANG_PATH = $libclang

    # ── LOCALE_ARCHIVE ───────────────────────────────────────────────────────
    # glibc locale tools look for this. The profile exposes it at a stable path
    # when glibc-locales is in the profile; if not present we leave it unset
    # rather than pointing at a path that doesn't exist.
    let locale_archive = $"($profile)/lib/locale/locale-archive"
    if ($locale_archive | path exists) {
        $env.LOCALE_ARCHIVE = $locale_archive
    }

    # ── PATH: profile bin ────────────────────────────────────────────────────
    # Prepend the profile bin directory so profile-installed tools win over
    # anything that might be in a system PATH. This covers coreutils, cargo,
    # rustc, pkg-config, clang, etc. without naming any store path.
    let profile_bin = $"($profile)/bin"
    if ($profile_bin | path exists) {
        $env.PATH = ($env.PATH | split row ":" | prepend $profile_bin | uniq | str join ":")
    }

    # ── CARGO_TARGET_DIR ─────────────────────────────────────────────────────
    # Shared build cache across pipeline runs. Warm incremental builds are
    # meaningfully faster; the tradeoff is disk usage in /var/cache.
    let cargo_target = "/var/cache/cargo-target"
    mkdir $cargo_target
    ^chmod 0755 $cargo_target
    $env.CARGO_TARGET_DIR = $cargo_target

    # ── nix.conf: trust the CI runner ────────────────────────────────────────
    # Without extra-trusted-users the nix daemon rejects unsigned builds from
    # any uid that isn't root or a nixbld user.
    let ci_user = (^whoami | str trim)
    let nix_conf = "/etc/nix/nix.conf"
    let already_trusted = (try { open --raw $nix_conf | str contains $ci_user } catch { false })
    if not $already_trusted {
        $"
extra-trusted-users = ($ci_user)
" | save --append $nix_conf
    }

    # ── aarch64: sandbox flags ────────────────────────────────────────────────
    # QEMU-backed aarch64 runners fail with the default seccomp sandbox.
    # We disable unconditionally on aarch64 rather than probing /proc/cpuinfo,
    # which is fragile across kernel and QEMU versions.
    let arch = (^uname -m | str trim)
    if $arch == "aarch64" {
        let already = (try { open --raw $nix_conf | str contains "aarch64-linux" } catch { false })
        if not $already {
            "
system = aarch64-linux
extra-platforms = x86_64-linux
sandbox = false
filter-syscalls = false
"
            | save --append $nix_conf
        }
    }

    # ── nixbld group + build users ────────────────────────────────────────────
    # The nix daemon requires these in /etc/passwd and /etc/group before it
    # will start. They already exist if the container launched via start.nu;
    # we create them here for the direct-entrypoint case.
    let cpus = (^nproc | str trim | into int)
    let dummy_shell = if ("/bin/nologin" | path exists) { "/bin/nologin" } else { "/bin/false" }

    let nixbld_in_group = (try { open --raw /etc/group | str contains "nixbld:" } catch { false })
    if not $nixbld_in_group {
        "nixbld:x:30000:
" | save --append /etc/group
        "nixbld:x::
"      | save --append /etc/gshadow
        mkdir /var/empty
    }

    let members = (seq 1 $cpus | each {|i|
        let mname = $"nixbld($i)"
        let exists = (try { open --raw /etc/passwd | str contains $"($mname):" } catch { false })
        if not $exists {
            let muid = 30000 + $i
            $"($mname):x:($muid):30000:Nix build user ($i):/var/empty:($dummy_shell)
"
            | save --append /etc/passwd
        }
        $mname
    })

    open --raw /etc/group
    | lines
    | where { not ($in | str starts-with "nixbld:") }
    | append $"nixbld:x:30000:($members | str join ',')"
    | str join "
"
    | $"($in)
"
    | save --force /etc/group

    log-warn "cargo install cargo-cyclonedx --quiet" --component $COMPONENT
    cargo install cargo-cyclonedx --quiet

    log-info "container environment bootstrapped" --component $COMPONENT
}

# Start vigild and block until the nix-daemon socket is ready.
#
# Must be called before any `nix build` invocation (Phase 3). Deliberately
# separated from bootstrap-container-env so that --skip-images runs pay zero
# daemon startup cost.
#
# Incorporates the full set of daemon prerequisites from start.nu:
#   - nixbld group + build users (nix-daemon refuses to start without them)
#   - gshadow rewrite (parity with start.nu; some PAM configs read this)
#   - db.sqlite symlink fixup (Docker overlay quirk: nix-daemon rejects a
#     symlink where it expects a regular file for the store DB)
#   - vigild supervision layer (only the nix-daemon service — no Dropbear,
#     no SSH, none of the interactive scaffolding from start.nu)
#
# Returns without error if vigild is already running (idempotent).
def start-nix-daemon [] {
    if ("/run/vigil/vigild.sock" | path exists) {
        log-debug "vigild already running, skipping daemon start" --component $COMPONENT
        return
    }

    # ── nixbld users ─────────────────────────────────────────────────────────
    # nix-daemon forks build processes as nixbld(N) users. They must exist in
    # /etc/passwd and /etc/group before the daemon starts or it will refuse to
    # spawn builders. start.nu created these; we recreate them here for the
    # direct-entrypoint path.
    let cpus        = (^nproc | str trim | into int)
    let dummy_shell = if ("/bin/nologin" | path exists) { "/bin/nologin" } else { "/bin/false" }
    let ci_user     = (^whoami | str trim)

    # Append group/gshadow stubs only if they don't already exist.
    let nixbld_in_group = (try { open --raw /etc/group | str contains "nixbld:" } catch { false })
    if not $nixbld_in_group {
        "nixbld:x:30000:
"  | save --append /etc/group
        "nixbld:x::
"       | save --append /etc/gshadow
        mkdir /var/empty
    }

    let members = (seq 1 $cpus | each {|i|
        let muid  = 30000 + $i
        let mname = $"nixbld($i)"
        let exists = (try { open --raw /etc/passwd | lines | any { str starts-with $"($mname):" } } catch { false })
        if not $exists {
            $"($mname):x:($muid):30000:Nix build user ($i):/var/empty:($dummy_shell)
"
            | save --append /etc/passwd
        }
        $mname
    })

    let member_list = ($members | str join ",")

    # Rewrite the full nixbld line in /etc/group and /etc/gshadow so the
    # member list is complete even if some entries were added incrementally.
    open --raw /etc/group
    | lines
    | where { not ($in | str starts-with "nixbld:") }
    | append $"nixbld:x:30000:($member_list)"
    | str join "
"
    | $"($in)
"
    | save --force /etc/group

    open --raw /etc/gshadow
    | lines
    | where { not ($in | str starts-with "nixbld:") }
    | append $"nixbld:!:($member_list):"
    | str join "
"
    | $"($in)
"
    | save --force /etc/gshadow

    # ── nix.conf: trust the current user ─────────────────────────────────────
    # Duplicates the bootstrap-container-env check as a belt-and-suspenders
    # guard: if the daemon starts before the conf is written it will reject
    # our builds, so we ensure it's set immediately before launch.
    let nix_conf = "/etc/nix/nix.conf"
    let already_trusted = (try { open --raw $nix_conf | str contains $ci_user } catch { false })
    if not $already_trusted {
        $"
extra-trusted-users = ($ci_user)
" | save --append $nix_conf
    }

    # ── db.sqlite symlink fixup ───────────────────────────────────────────────
    # When Nix is installed into a Docker image, the store DB is sometimes
    # bind-mounted as a symlink through the overlay filesystem. The nix-daemon
    # requires regular files at /nix/var/nix/db/db.sqlite (and siblings) and
    # will refuse to start if it finds symlinks. We detect this case and
    # materialise the files in-place.
    if ("/nix/var/nix/db/db.sqlite" | path type) == "symlink" {
        log-info "nix db.sqlite is a symlink — materialising for daemon compatibility" --component $COMPONENT
        let db_src = ("/nix/var/nix/db/db.sqlite" | path expand | path dirname)
        let tmp    = (^mktemp -d | str trim)

        ls $db_src | get name | each {|f| cp --preserve [] $f $tmp; null }

        for f in ["db.sqlite" "db.sqlite-shm" "db.sqlite-wal" "big-lock" "reserved" "schema"] {
            let p = $"/nix/var/nix/db/($f)"
            if ($p | path exists) { rm -f $p }
        }

        ls $tmp | get name | each {|f| cp --preserve [] $f /nix/var/nix/db/; null }
        rm -rf $tmp

        # Permissions mirror what nix-daemon expects: DB files world-readable,
        # lock files root-only.
        ^chmod 644 /nix/var/nix/db/db.sqlite
        ^chmod 644 /nix/var/nix/db/db.sqlite-shm
        ^chmod 644 /nix/var/nix/db/db.sqlite-wal
        ^chmod 644 /nix/var/nix/db/schema
        ^chmod 600 /nix/var/nix/db/big-lock
        ^chmod 600 /nix/var/nix/db/reserved
    }

    # ── vigild: nix-daemon supervision ───────────────────────────────────────
    # CI-only layer: just nix-daemon. No Dropbear, no interactive services.
    mkdir /run/vigil/layers

    "summary: ci background services

services:
  nix-daemon:
    summary: Nix build daemon
    command: /bin/nix-daemon --daemon
    startup: enabled
    on-success: restart
    on-failure: restart
" | save --force /run/vigil/layers/001-ci.yaml

    job spawn { ^/bin/vigild --layers-dir /run/vigil/layers --socket /run/vigil/vigild.sock }

    let vigild_ready = (
        1..20 | each {|_| sleep 100ms; "/run/vigil/vigild.sock" | path exists }
        | any {|x| $x }
    )
    if not $vigild_ready {
        log-warn "vigild did not become ready within 2s — nix builds may fail" --component $COMPONENT
        return
    }
    ^chmod 666 /run/vigil/vigild.sock

    let daemon_ready = (
        1..30 | each {|_| sleep 200ms; "/nix/var/nix/daemon-socket/socket" | path exists }
        | any {|x| $x }
    )
    if not $daemon_ready {
        log-warn "nix-daemon socket did not appear within 6s — nix build may fail" --component $COMPONENT
    } else {
        ^chmod 666 /nix/var/nix/daemon-socket/socket
        log-info "nix-daemon ready" --component $COMPONENT
    }
}

# ===========================================================================
# Phase 0: Static analysis
# ===========================================================================

# ANSI helpers — used only for static analysis terminal output.
def green  [msg: string] { $"(ansi green)($msg)(ansi reset)" }
def red    [msg: string] { $"(ansi red)($msg)(ansi reset)" }
def yellow [msg: string] { $"(ansi yellow)($msg)(ansi reset)" }
def bold   [msg: string] { $"(ansi attr_bold)($msg)(ansi reset)" }

# Abort early if any required tool is absent from PATH.
def check-tools [tools: list<string>] {
    let missing = ($tools | where { |t| (which $t | is-empty) })
    if ($missing | is-not-empty) {
        log-error $"missing required tools: ($missing | str join ', ')" --component $COMPONENT
        error make { msg: "preflight tool check failed" }
    }
}

# Run one tool, capturing stdout+stderr to outfile.
# Returns { name, outfile, exit_code, duration_sec }.
#
# The original static-tools.nu ran each command twice (two do --ignore-errors
# blocks, the first result immediately overwritten). Collapsed to one invocation.
def run-tool [
    name:    string
    outfile: string
    cmd:     list<string>
]: nothing -> record {
    let start = (date now)
    log-info (yellow $"  → ($name)...") --component $COMPONENT

    let bin  = $cmd.0
    let args = ($cmd | skip 1)

    let exit_code = (do --ignore-errors {
        ^$bin ...$args out+err> $outfile
        0
    } | if ($in | is-empty) { 1 } else { $in | into int })

    let duration = (((date now) - $start) / 1sec | math round)

    { name: $name, outfile: $outfile, exit_code: $exit_code, duration_sec: $duration }
}

# Data-driven tool manifest. Separating definitions from execution lets
# par-each run them cleanly and makes add/remove/reorder a one-liner.
#
# cargo mutants is excluded here — it spawns its own worker pool and does
# heavy disk I/O; running it alongside everything else causes contention.
# It runs sequentially after this batch, as does noseyparker (own datastore).
def static-tool-definitions [manifest: string, output_dir: string]: nothing -> list<record> {
    [
        {
            name:    "cargo deny"
            outfile: $"($output_dir)/cargo_deny.txt"
            cmd:     [cargo deny --manifest-path $manifest check]
        }
        {
            name:    "cargo clippy"
            outfile: $"($output_dir)/cargo_clippy.txt"
            cmd:     [cargo clippy --manifest-path $manifest --all-targets --all-features -- -D warnings]
        }
        {
            name:    "cargo udeps"
            outfile: $"($output_dir)/cargo_udeps.txt"
            cmd:     [cargo udeps --manifest-path $manifest --all-targets]
        }
        {
            name:    "cargo semver-checks"
            outfile: $"($output_dir)/cargo_semver.txt"
            cmd:     [cargo semver-checks --manifest-path $manifest]
        }
        {
            name:    "cargo geiger"
            outfile: $"($output_dir)/cargo_geiger.txt"
            cmd:     [cargo geiger --manifest-path $manifest]
        }
        {
            name:    "cargo hack (feature powerset)"
            outfile: $"($output_dir)/cargo_hack.txt"
            cmd:     [cargo hack check --manifest-path $manifest --feature-powerset]
        }
    ]
}

def count-pattern [file: string, pattern: string]: nothing -> int {
    if not ($file | path exists) { return 0 }
    open $file | lines | where { |l| $l =~ $pattern } | length
}

def generate-static-summary [results: table, output_dir: string] {
    let summary_file = $"($output_dir)/static-analysis-summary.txt"

    mut sections = [(bold "Polar Static Analysis Summary") ""]

    $sections = ($sections | append [(bold "=== Tool Results ===") ($results | select name exit_code duration_sec | to text) ""])

    let clippy_warnings = (count-pattern $"($output_dir)/cargo_clippy.txt" 'warning\[')
    let clippy_errors   = (count-pattern $"($output_dir)/cargo_clippy.txt" 'error\[')
    $sections = ($sections | append [(bold "=== cargo clippy ===") $"Warnings: ($clippy_warnings)  Errors: ($clippy_errors)" ""])

    let deny_errors = (count-pattern $"($output_dir)/cargo_deny.txt" '^error')
    $sections = ($sections | append [(bold "=== cargo deny ===") $"Issues: ($deny_errors)" ""])

    let udeps_file = $"($output_dir)/cargo_udeps.txt"
    let udeps_lines = if ($udeps_file | path exists) {
        open $udeps_file | lines | where { |l| $l =~ 'unused|crate' } | first 10
    } else { [] }
    let udeps_body = if ($udeps_lines | is-not-empty) { $udeps_lines | str join "\n" } else { "No unused dependencies found." }
    $sections = ($sections | append [(bold "=== cargo udeps ===") $udeps_body ""])

    let semver_file = $"($output_dir)/cargo_semver.txt"
    let semver_body = if ($semver_file | path exists) {
        open $semver_file | lines | where { |l| $l =~ 'Parsed|Checked|Summary|semver' } | str join "\n"
    } else { "" }
    $sections = ($sections | append [(bold "=== cargo semver-checks ===") $semver_body ""])

    let geiger_file = $"($output_dir)/cargo_geiger.txt"
    let geiger_body = if ($geiger_file | path exists) { open $geiger_file | lines | last 15 | str join "\n" } else { "" }
    $sections = ($sections | append [(bold "=== cargo geiger ===") $geiger_body ""])

    let np_report = $"($output_dir)/noseyparker_report.txt"
    let np_body = if ($np_report | path exists) { open $np_report | lines | first 30 | str join "\n" } else { "" }
    $sections = ($sections | append [(bold "=== noseyparker ===") $np_body ""])

    let mutants_file = $"($output_dir)/cargo_mutants.txt"
    let mutants_body = if ($mutants_file | path exists) {
        open $mutants_file | lines | where { |l| $l =~ 'caught|missed|unviable|timeout|ok' } | last 10 | str join "\n"
    } else { "" }
    $sections = ($sections | append [(bold "=== cargo mutants ===") $mutants_body ""])

    let failed = ($results | where exit_code != 0)
    if ($failed | is-not-empty) {
        let callout = ($failed | each { |r| $"  ($r.name) — exit ($r.exit_code) — ($r.outfile)" } | str join "\n")
        $sections = ($sections | append [(bold "=== Tools with non-zero exit ===") $callout ""])
    }

    $sections | flatten | str join "\n" | save --force $summary_file
    log-info (green $"summary written to ($summary_file)") --component $COMPONENT
}

# Run all static analysis phases and return the aggregated results table.
# Caller decides whether to abort based on --strict-analysis.
def run-static-analysis [ws_manifest: path, artifact_dir: path]: nothing -> table {
    check-tools [
        cargo cargo-deny cargo-udeps cargo-semver-checks
        cargo-geiger cargo-hack cargo-mutants noseyparker-cli
    ]

    log-info (bold "phase 0 — static analysis") --component $COMPONENT

    # Parallel batch — tools that are IO-independent of each other.
    let parallel_results = (
        static-tool-definitions ($ws_manifest | path expand | into string) ($artifact_dir | into string)
        | par-each {|tool| run-tool $tool.name $tool.outfile $tool.cmd }
    )

    # noseyparker: manages its own datastore, must run sequentially.
    log-info "  → noseyparker..." --component $COMPONENT
    let np_datastore = $"($artifact_dir)/noseyparker_datastore"
    let np_scan_out  = $"($artifact_dir)/noseyparker_scan.txt"
    let np_report    = $"($artifact_dir)/noseyparker_report.txt"
    rm -rf $np_datastore

    let np_scan = (run-tool "noseyparker scan" $np_scan_out [noseyparker-cli scan --datastore $np_datastore .])
    # Report exits 0 always; findings live in the file.
    noseyparker-cli report --datastore $np_datastore out+err> $np_report
    let np_report_result = { name: "noseyparker report", outfile: $np_report, exit_code: 0, duration_sec: 0 }

    # cargo mutants: spawns its own worker pool, must run sequentially last.
    log-info "  → cargo mutants (slow)..." --component $COMPONENT
    let mutants_out     = $"($artifact_dir)/cargo_mutants.txt"
    let mutants_out_dir = $"($artifact_dir)/mutants.out"
    let mutants = (run-tool "cargo mutants" $mutants_out [
        cargo mutants --manifest-path ($ws_manifest | into string) --output $mutants_out_dir
    ])

    let all_results = ($parallel_results | append $np_scan | append $np_report_result | append $mutants)

    log-info ($all_results | select name exit_code duration_sec | table) --component $COMPONENT
    generate-static-summary $all_results ($artifact_dir | into string)

    $all_results
}

# ===========================================================================
# Image manifest — single source of truth for OCI images in this project.
#
# `root_purl` is the join key to the source-level SBOM. Leave it empty for
# images that don't correspond to a workspace crate (third-party bases, etc.);
# the image.linked event will be emitted without package linkage.
# ===========================================================================

def image-manifest []: nothing -> list<record> {
    [
        { name: "cassini",         flake: ".#cassiniImage",            image: "cassini",                 root_purl: "pkg:cargo/cassini@0.1.0"            }
        { name: "gitlab-observer", flake: ".#gitlabObserverImage",     image: "polar-gitlab-observer",   root_purl: "pkg:cargo/gitlab-observer@0.1.0"    }
        { name: "gitlab-consumer", flake: ".#gitlabConsumerImage",     image: "polar-gitlab-consumer",   root_purl: "pkg:cargo/gitlab-consumer@0.1.0"    }
        { name: "kube-observer",   flake: ".#kubeObserverImage",       image: "polar-kube-observer",     root_purl: "pkg:cargo/kube-observer@0.1.0"      }
        { name: "kube-consumer",   flake: ".#kubeConsumerImage",       image: "polar-kube-consumer",     root_purl: "pkg:cargo/kube-consumer@0.1.0"      }
        { name: "git-observer",    flake: ".#gitObserverImage",        image: "polar-git-observer",      root_purl: "pkg:cargo/git-repo-observer@0.1.0"  }
        { name: "git-consumer",    flake: ".#gitConsumerImage",        image: "polar-git-consumer",      root_purl: ""                                   }
        { name: "git-scheduler",   flake: ".#gitSchedulerImage",       image: "polar-git-scheduler",     root_purl: ""                                   }
        { name: "linker",          flake: ".#provenanceLinkerImage",   image: "polar-linker-agent",      root_purl: "pkg:cargo/provenance-linker@0.1.0"  }
        { name: "resolver",        flake: ".#provenanceResolverImage", image: "polar-resolver-agent",    root_purl: "pkg:cargo/provenance-resolver@0.1.0"}
    ]
}

# ===========================================================================
# Cargo: package resolution
# ===========================================================================

def resolve-packages [
    --package (-p): string = ""
]: nothing -> list<record> {
    let ws_root   = (workspace-root)
    let manifest  = ($ws_root | path join "Cargo.toml")
    let meta      = (
        cargo metadata --manifest-path $manifest --format-version 1 --no-deps
        | from json
    )

    let packages = if ($package | is-empty) {
        let members = ($meta.workspace_members | default [])
        $meta.packages | where {|pkg| $members | any {|id| $id == $pkg.id } }
    } else {
        $meta.packages | where name == $package
    }

    if ($packages | is-empty) {
        error make { msg: $"no Cargo package matched '($package)'" }
    }

    $packages | each {|pkg| {
        name:          $pkg.name
        manifest_path: ($pkg.manifest_path | path expand)
        version:       ($pkg.version? | default "0.0.0")
        purl:          $"pkg:cargo/($pkg.name)@($pkg.version? | default '0.0.0')"
    }}
}

# ===========================================================================
# Phase 1: SBOM generation and graph projection
# ===========================================================================

# Run cargo-cyclonedx across the workspace and collect the resulting files
# into artifact_dir. Returns the list of file records from ls.
def generate-workspace-sboms [
    packages:     list<record>
    ws_manifest:  path
    artifact_dir: path
    --target (-t): string = ""
]: nothing -> list<record> {
    let ws_root = ($ws_manifest | path dirname)

    log-info $"generating SBOMs for ($packages | length) package\(s\)" --component $COMPONENT

    mut args = [--manifest-path $ws_manifest -f json]
    if ($target | is-not-empty) { $args = ($args | append [--target $target]) }
    log-info $"running: cargo cyclonedx ($args | str join ' ')" --component $COMPONENT

    let result = (^cargo cyclonedx ...$args | complete)
    if $result.exit_code != 0 {
        let msg = ($result.stderr | default $result.stdout | str trim)
        log-warn $"cargo-cyclonedx failed: ($msg)" --component $COMPONENT
        return []
    }

    # cargo-cyclonedx drops each SBOM next to its crate's Cargo.toml.
    # Glob the entire workspace tree and move every *.cdx.json into
    # artifact_dir. Pure Nu — no sh, no quoting hazards, no silent swallow.
    let found = (glob $"($ws_root)/**/*.cdx.json" | where { ($in | path type) == "file" })

    log-debug $"found ($found | length) SBOM file\(s\) under ($ws_root)" --component $COMPONENT

    if ($found | is-empty) {
        log-warn "cargo-cyclonedx reported success but produced no *.cdx.json files" --component $COMPONENT
        return []
    }

    for src in $found {
        let dest = ($artifact_dir | path join ($src | path basename))
        if ($src | path expand) == ($dest | path expand) {
            log-debug $"skipping ($src) — already in artifact dir" --component $COMPONENT
            continue
        }
        log-debug $"moving ($src) -> ($dest)" --component $COMPONENT
        mv --force $src $dest
    }

    let collected = (
        ls $artifact_dir
        | where type == file
        | where { ($in.name | path basename | str ends-with ".cdx.json") }
    )

    log-debug $"($collected | length) SBOM\(s\) staged in ($artifact_dir):" --component $COMPONENT
    for f in $collected { log-debug $"  ($f.name)" --component $COMPONENT }

    $collected
}

# Parse each SBOM file, project it into a graph fragment, and emit events.
#
# Returns a record keyed by package name (SBOM stem) mapping to:
#   { content_hash, root_purl, component_count, edge_count }
#
# This lookup flows directly into Phase 2 (binary linking) and Phase 3
# (image.linked events) in-process — no file serialisation required.
def process-cargo-sboms [
    sbom_files: list<record>
    packages:   list<record>
]: nothing -> record {
    mut sbom_lookup = {}

    for f in $sbom_files {
        let doc = try { open $f.name } catch {|e|
            log-warn $"could not parse ($f.name): ($e.msg)" --component $COMPONENT
            continue
        }
        if ($doc.bomFormat? | default "") != "CycloneDX" { continue }

        let filename     = ($f.name | path basename)
        let stem         = ($filename | str replace ".cdx.json" "")
        let content_hash = (content-hash-file $f.name)

        emit-artifact-produced $content_hash "sbom" --name $filename --content_type "application/vnd.cyclonedx+json"

        let fragment = (extract-graph-fragment $doc $content_hash)

        if $fragment.root != null {
            emit-sbom-analyzed $fragment $filename
            log-info $"($stem): ($fragment.components | length) components, ($fragment.edges | length) edges" --component $COMPONENT
        } else {
            log-warn $"($filename) has no metadata.component — graph fragment not emitted" --component $COMPONENT
        }

        let matched   = ($packages | where name == $stem | first | default null)
        let root_purl = if $fragment.root != null {
            $fragment.root.purl
        } else if $matched != null {
            ($matched.purl? | default "")
        } else {
            ""
        }

        $sbom_lookup = ($sbom_lookup | insert $stem {
            content_hash:    $content_hash
            root_purl:       $root_purl
            component_count: ($fragment.components | length)
            edge_count:      ($fragment.edges | length)
        })
    }

    $sbom_lookup
}

# ===========================================================================
# Phase 2: Binary build and provenance linking
# ===========================================================================

def emit-binary-linked [
    binary_content_hash: string
    binary_name:         string
    root_purl:           string
    sbom_content_hash:   string
    --binding_digest:    string = ""
] {
    mut payload = {
        binary_content_hash: $binary_content_hash
        binary_name:         $binary_name
        root_purl:           $root_purl
        sbom_content_hash:   $sbom_content_hash
    }
    if ($binding_digest | is-not-empty) {
        $payload = ($payload | insert binding_digest $binding_digest)
    }
    emit "binary.linked" $payload
}

def build-and-link-binaries [
    packages:         list<record>
    sbom_lookup:      record
    artifact_dir:     path
    --release
    --target:         string = ""
    --filter_package: string = ""
]: nothing -> list<record> {
    let ws_root = (workspace-root)

    mut cargo_args = ["build" "--message-format=json" "--locked" "--quiet"]
    if ($filter_package | is-not-empty) { $cargo_args = ($cargo_args | append ["--package" $filter_package]) }
    if $release                         { $cargo_args = ($cargo_args | append "--release") }
    if ($target | is-not-empty)         { $cargo_args = ($cargo_args | append ["--target" $target]) }

    log-info $"running: cargo ($cargo_args | str join ' ')" --component $COMPONENT

    let cargo_lock_hash  = (content-hash-file $"($ws_root)/Cargo.lock")
    let source_tree_hash = (tree-hash $ws_root)

    let binaries = (
        cargo ...$cargo_args
        | lines
        | where { ($in | str trim) != "" }
        | each {|line| try { $line | from json } catch { null } }
        | where { $in != null }
        | where { ($in.reason? | default "") == "compiler-artifact" }
        | where { ($in.target?.kind? | default []) | any {|k| $k == "bin"} }
        | where { ($in.executable? | default null) != null }
        | each {|artifact|
            let exe         = $artifact.executable
            let name        = ($exe | path basename)
            let dest        = ($artifact_dir | path join $name)
            let binary_hash = (content-hash-file $exe)

            mv $exe $dest

            # manifest_path is the stable match key — target.name is the
            # binary name (not the package name) and package_id is version-sensitive.
            let pkg_manifest = ($artifact.manifest_path? | default "")
            let pkg          = ($packages | where manifest_path == $pkg_manifest | first | default null)

            if $pkg == null {
                log-warn $"($name): no matching package for manifest ($pkg_manifest)" --component $COMPONENT
                return null
            }

            emit-artifact-produced $binary_hash $ELF_BINARY_ARTIFACT --name $name

            let sbom_info = ($sbom_lookup | get -o $pkg.name | default null)

            if $sbom_info != null {
                let cargo_toml_hash = (content-hash-file $pkg.manifest_path)
                let binding = (
                    [$binary_hash $cargo_toml_hash $cargo_lock_hash $source_tree_hash]
                    | str join ":"
                    | hash sha256
                    | $"sha256:($in)"
                )
                emit-binary-linked $binary_hash $name $sbom_info.root_purl $sbom_info.content_hash --binding_digest $binding
                log-info $"($name) -> ($sbom_info.root_purl)" --component $COMPONENT
            } else {
                log-warn $"($name): no SBOM for package ($pkg.name) — binary.linked not emitted" --component $COMPONENT
            }

            {
                name:              $name
                path:              $dest
                binary_hash:       $binary_hash
                package_name:      $pkg.name
                root_purl:         ($sbom_info.root_purl?    | default "")
                sbom_content_hash: ($sbom_info.content_hash? | default "")
            }
        }
        | where { $in != null }
    )

    if ($env.LAST_EXIT_CODE? | default 0) != 0 {
        error make { msg: "cargo build failed" }
    }

    $binaries
}

# ===========================================================================
# Phase 3: OCI image build, scan, and upload
# ===========================================================================

# Construct skopeo destination refs for a given image name.
# Returns a list of ref templates with the literal string `{tag}` as
# the placeholder; callers substitute the actual tag via str replace.
def registry-refs [image_name: string]: nothing -> list<string> {
    mut refs = []
    let ci_reg   = ($env.CI_REGISTRY_IMAGE? | default "")
    let azure_reg = ($env.AZURE_REGISTRY?   | default "")
    if ($ci_reg   | is-not-empty) { $refs = ($refs | append $"docker://($ci_reg)/($image_name):{tag}") }
    if ($azure_reg | is-not-empty) { $refs = ($refs | append $"docker://($azure_reg)/($image_name):{tag}") }
    $refs
}

# Assemble registry credentials from environment and log in via skopeo.
# Runs only when --skip-upload is not set.
def login-to-registries [] {
    mut creds = []

    let ci_user = ($env.CI_REGISTRY_USER?     | default "")
    let ci_pass = ($env.CI_REGISTRY_PASSWORD?  | default "")
    let ci_reg  = ($env.CI_REGISTRY?           | default "")
    if ($ci_user | is-not-empty) and ($ci_reg | is-not-empty) {
        $creds = ($creds | append { registry: $ci_reg, username: $ci_user, password: $ci_pass })
    }

    let acr_user = ($env.ACR_USERNAME?  | default "")
    let acr_pass = ($env.ACR_TOKEN?     | default "")
    let acr_reg  = ($env.AZURE_REGISTRY? | default "")
    if ($acr_user | is-not-empty) and ($acr_reg | is-not-empty) {
        $creds = ($creds | append { registry: $acr_reg, username: $acr_user, password: $acr_pass })
    }

    if ($creds | is-not-empty) {
        registry-login $creds
    }
}

# Orchestrate the full lifecycle for a single image: build → upload → sign.
#
# Image SBOMs are intentionally omitted — syft guesses at Nix store contents
# by parsing paths rather than reading derivation metadata, producing noisy
# results that aren't worth the runtime cost.
# See: https://github.com/cmu-sei/Polar/issues/178
#
# --skip-upload: build the tarball only (useful for local testing / cache warm)
# Default:       build → upload to all registries → sign each pushed digest
def process-image [
    entry:     record   # Row from image-manifest
    image_tag: string
    --skip-upload
]: nothing -> record {
    let build = (nix-build-image $entry.flake $entry.name)
    if not $build.success { return { success: false, image_name: $entry.name, uploads: [] } }

    if $skip_upload {
        return { success: true, image_name: $entry.name, uploads: [] }
    }

    build-and-push-image $entry.name $image_tag (registry-refs $entry.image) $build --root_purl $entry.root_purl
}

# Build → upload → sign for one image. Lives in ci.nu rather than core.nu
# because this is project-specific orchestration, not a reusable primitive.
#
# Sequence:
#   1. skopeo copy tarball to each registry
#   2. cosign sign each pushed digest  → emits image.signed
#   3. emit image.linked               → ties registry digest to root purl
#
# The nix build and OCI metadata extraction are done by the caller
# (process-image) so we don't re-crack the tarball here.
def build-and-push-image [
    link_name:  string
    tag:        string
    registries: list<string>
    build:      record          # Result record from nix-build-image
    --root_purl: string = ""
]: nothing -> record {
    let oci_metadata = $build.oci_metadata
    let has_oci      = ($oci_metadata | get -o success | default false)

    let uploads = ($registries | each {|template|
        upload-image $build.tarball ($template | str replace "{tag}" $tag) --name $link_name
    })

    for upload in $uploads {
        if ($upload.digest | is-empty) {
            log-warn $"no digest for ($upload.remote_ref) — skipping sign and image.linked" --component $COMPONENT
            continue
        }

        # cosign requires a digest ref: registry.io/org/app@sha256:abc...
        # remote_ref is "docker://registry.io/org/app:tag" — strip the scheme and swap the tag.
        let base_ref   = ($upload.remote_ref | str replace "docker://" "" | parse "{repo}:{tag}" | get -o 0 | default { repo: "" } | get repo)
        let digest_ref = $"($base_ref)@($upload.digest)"
        let sign       = (sign-image $digest_ref --name $upload.name)

        if $sign.success {
            emit "image.signed" {
                image_digest:    $upload.digest
                remote_ref:      $upload.remote_ref
                image_name:      $link_name
                signing_key_ref: ($env.COSIGN_KEY? | default "")
                config_digest:   ($oci_metadata.config_digest? | default "")
            }
        }

        if ($root_purl | is-not-empty) {
            emit "image.linked" {
                image_digest:   $upload.digest
                remote_ref:     $upload.remote_ref
                image_name:     $link_name
                root_purl:      $root_purl
                config_digest:  (if $has_oci { $oci_metadata.config_digest } else { "" })
                layer_manifest: (if $has_oci { $oci_metadata.layers }        else { [] })
                os:             (if $has_oci { $oci_metadata.os }             else { "" })
                arch:           (if $has_oci { $oci_metadata.arch }           else { "" })
            }
        }
    }

    { success: true, image_name: $link_name, tarball: $build.tarball, oci_metadata: $oci_metadata, uploads: $uploads }
}

# ===========================================================================
# Main
# ===========================================================================

def main [
    # Cargo flags
    --package (-p): string = ""   # Restrict cargo phases to a single workspace member
    --release (-r)                # Pass --release to cargo build
    --target (-t):  string = ""   # Cross-compilation target triple

    # Image flags
    --tag:          string = ""   # Image tag; defaults to CI_COMMIT_SHORT_SHA or "latest"
    --filter:       string = ""   # Only build images whose name contains this string
    --skip-upload                 # Build tarball only, skip push to registries

    # Shared
    --artifact-dir:   path = "pipeline-out"
    --skip-analysis               # Skip Phase 0 static analysis entirely
    --strict-analysis             # Abort pipeline if any static analysis tool exits non-zero
    --skip-build                  # Skip cargo binary compilation (SBOM generation still runs)
    --skip-images                 # Skip Phase 3 entirely
] {
    # Bootstrap must run before anything else — sets OpenSSL env vars that
    # cargo needs. It is fast and idempotent so there is no harm running it
    # even when the container was launched via start.nu.
    bootstrap-container-env

    mkdir -v $artifact_dir

    let ws_root     = (workspace-root)
    let ws_manifest = ($ws_root | path join "Cargo.toml")

    # ------------------------------------------------------------------
    # Phase 0 — Static analysis (before Cassini; no provenance events)
    # ------------------------------------------------------------------
    if not $skip_analysis {
        let analysis_results = (run-static-analysis $ws_manifest $artifact_dir)
        let failed = ($analysis_results | where exit_code != 0)
        if ($failed | is-not-empty) {
            let names = ($failed | get name | str join ", ")
            if $strict_analysis {
                error make { msg: $"static analysis failed (strict mode): ($names)" }
            } else {
                log-warn $"static analysis non-zero exits: ($names) — continuing (pass --strict-analysis to abort)" --component $COMPONENT
            }
        }
    }

    let cassini_job_id = (start-cassini-daemon)

    let image_tag  = if ($tag | is-not-empty) { $tag } else { ($env.CI_COMMIT_SHORT_SHA? | default "latest") }
    let packages   = (resolve-packages --package $package)

    log-info $"($packages | length) package\(s\) in scope, image tag: ($image_tag)" --component $COMPONENT

    # ------------------------------------------------------------------
    # Phase 1 — Cargo SBOMs
    # ------------------------------------------------------------------
    let sbom_files  = (generate-workspace-sboms $packages $ws_manifest $artifact_dir --target $target)
    let sbom_lookup = (process-cargo-sboms $sbom_files $packages)
    log-info $"($sbom_files | length) SBOM\(s\) generated and analyzed" --component $COMPONENT

    # ------------------------------------------------------------------
    # Phase 2 — Cargo binaries
    # ------------------------------------------------------------------
    if not $skip_build {
        let binaries = (
            build-and-link-binaries $packages $sbom_lookup $artifact_dir
                --release
                --target $target
                --filter_package $package
        )
        log-info $"($binaries | length) binary\(ies\) built and linked" --component $COMPONENT
    }

    # ------------------------------------------------------------------
    # Phase 3 — OCI images
    # ------------------------------------------------------------------
    if not $skip_images {
        # nix build requires the daemon. Start it now rather than at bootstrap
        # time so runs with --skip-images pay zero daemon overhead.
        start-nix-daemon
        let manifest = if ($filter | is-not-empty) {
            image-manifest | where { $in.name | str contains $filter }
        } else {
            image-manifest
        }

        if ($manifest | is-empty) {
            log-warn $"no images match filter '($filter)' — skipping Phase 3" --component $COMPONENT
        } else {
            if not $skip_upload { login-to-registries }

            log-info $"building ($manifest | length) image\(s\)" --component $COMPONENT

            let results = ($manifest | each {|entry|
                log-info $"--- ($entry.name) ---" --component $COMPONENT
                process-image $entry $image_tag --skip-upload=$skip_upload
            })

            let succeeded = ($results | where success == true  | length)
            let failed    = ($results | where success == false | length)
            log-info $"($succeeded) image\(s\) succeeded, ($failed) failed" --component $COMPONENT

            if $failed > 0 {
                let failures = ($results | where success == false | get image_name | str join ", ")
                log-warn $"failed images: ($failures)" --component $COMPONENT
            }
        }
    }

    stop-cassini-daemon $cassini_job_id
}
