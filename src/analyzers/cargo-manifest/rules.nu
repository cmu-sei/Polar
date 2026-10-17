# rules.nu — cargo-manifest analyzer rules.
#
# Pure: reads the extracted package in a workspace and returns findings as
# records. Never compiles, executes, or fetches anything — the package under
# analysis is untrusted, and these rules only need its manifest and layout.
#
# Each finding is {rule_id, message, path?}. rule_ids are stable across
# analyzer versions; they are the identity of a check in the graph.
#
# Rules — each one flags code that runs, or native code that links, at the
# consumer's build time, i.e. where a compromised dependency acts before any
# of its library code is ever called:
#   build-script  — a build script runs arbitrary code during `cargo build`
#   proc-macro    — the crate is a compiler plugin; it runs inside rustc
#   native-links  — the crate links a native library (`links = "..."`)

# Resolve the build script path the way cargo does: an explicit `build = "path"`
# wins, `build = false` disables it, otherwise `build.rs` at the root is implicit.
# Published (normalized) manifests usually make it explicit; older ones may not.
def build-script-path [workspace: path, package: record]: nothing -> oneof<string, nothing> {
    let declared = $package.build? | default null
    match ($declared | describe) {
        string => $declared
        "bool" => (if $declared { "build.rs" } else { null })
        _ => (
            if ($workspace | path join "build.rs" | path exists) { "build.rs" } else { null }
        )
    }
}

export def analyze-manifest [workspace: path]: nothing -> list<record> {
    let manifest_path = $workspace | path join "Cargo.toml"
    if not ($manifest_path | path exists) {
        error make {msg: $"no Cargo.toml at ($workspace) — not a cargo package"}
    }
    let manifest = open --raw $manifest_path | from toml
    let package = $manifest.package? | default {}
    let lib = $manifest.lib? | default {}

    mut findings = []

    let build = (build-script-path $workspace $package)
    if $build != null {
        $findings ++= [
            {
                rule_id: "build-script"
                message: $"package declares a build script \(($build)\) that runs at the consumer's build time"
                path: $build
            }
        ]
    }

    # `proc_macro` is the pre-2018 spelling; cargo still accepts both.
    if ($lib | get -o "proc-macro" | default false) or ($lib | get -o "proc_macro" | default false) {
        $findings ++= [
            {rule_id: "proc-macro", message: "package is a procedural macro; it executes inside the compiler at the consumer's build time", path: "Cargo.toml"}
        ]
    }

    let links = $package.links? | default null
    if $links != null {
        $findings ++= [
            {
                rule_id: "native-links"
                message: $"package links the native library '($links)'"
                path: "Cargo.toml"
            }
        ]
    }

    $findings
}
