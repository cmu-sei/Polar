-- src/analyzers/cargo-manifest/container.dhall
--
-- Hardened analyzer image: runs analyze.nu against an untrusted, already
-- verified package extracted at /workspace by the orchestrator's init container.
--
-- The image digest is this analyzer's identity in the graph
-- ((BuildJob)-[:ANALYZED { analyzer_image }]->(Package)), so it should change
-- only when the analysis changes. Hence:
--   - the prelude is pinned by revision and hash, never `main`;
--   - Minimal mode on the Micro layer: no shell, no Nix, no start script,
--     no toolchain whose drift would change the digest.
--
-- Runtime hardening (read-only root, dropped capabilities, non-root, writable
-- /tmp only) is applied by the orchestrator's job spec, not here.
let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/e2334448bd4bb6348a467244d474f907d3d0e36d/dhall/prelude.dhall
        sha256:b81e69ef2fe811bc853a8a9a0202c0af802f7cd53c78f95f67083bf3dceee86b

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "polar-analyzer-cargo-manifest"
  , entrypoint = Some "polar-analyze-cargo"
  , staticUid  = Some 65532
  , staticGid  = Some 65532
  , extraEnv   =
      [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
      , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
      ]
  }
