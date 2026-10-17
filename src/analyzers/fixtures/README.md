# Analyzer fixtures

## `toy-crate/`

A crate that is never built. It exists for its dependency graph: about 15
crates, small enough to run the whole analysis loop on a laptop, and chosen so
that each `cargo-manifest` rule has something to find:

| Dependency | Expected findings |
|---|---|
| `libc`, `proc-macro2`, `quote`, `serde`, `serde_core` | `build-script` |
| `serde_derive` | `proc-macro` |
| `libz-sys` | `build-script`, `native-links` |
| `cc`, `find-msvc-tools`, `itoa`, `pkg-config`, `shlex`, `syn`, `unicode-ident`, `vcpkg` | none |

So a complete run of the current analyzer over the toy crate is 15 successful
runs with 8 findings, checked by running the rules on the locked archives.

`Cargo.lock` is committed, so the archive checksums the orchestrator verifies
are fixed. To change the set, edit `Cargo.toml` and run `cargo generate-lockfile`
in this directory.

## `seed-toy-sbom.nu`

Puts the toy crate's dependency graph into the knowledge graph the way an
instrumented pipeline does. It generates the SBOM with `cargo-cyclonedx` and
publishes one `SbomAnalyzed` event through `scripts/pipeline/core`. The build
processor then writes a `Package` node with an `archive_sha256` for each
dependency, which is what the analysis scheduler reads as its work queue.

```sh
nu src/analyzers/fixtures/seed-toy-sbom.nu --dry-run   # list what would be sent
nu src/analyzers/fixtures/seed-toy-sbom.nu             # publish (needs a broker)
```

Publishing needs `cassini-client` on `PATH` and the usual broker environment
(`BROKER_ADDR`, `CASSINI_SERVER_NAME`, `TLS_CLIENT_CERT`, `TLS_CLIENT_KEY`,
`TLS_CA_CERT`).
