#!/usr/bin/env nu
# infra/layers/2-services/neo4j/setup-neo4j.nu
#
# Neo4j init container script. Runs as polar-nu-init:latest.
# Mounted at /scripts/init.nu in the init container pod spec.
#
# Responsibilities:
#   1. Copy neo4j.conf from the ConfigMap mount to $NEO4J_HOME/conf/
#      (Neo4j requires the config file to be writable; ConfigMap mounts
#      are read-only in Kubernetes.)
#   2. Write TLS certificates from environment variables into the directory
#      structure Neo4j expects for mTLS on both HTTPS and Bolt.
#   3. Ensure neo4j (uid/gid 7474) owns all writable directories.
#
# Environment variables required for TLS:
#   TLS_SERVER_CERT_CONTENT  — PEM content of the Neo4j server certificate
#   TLS_SERVER_KEY_CONTENT   — PEM content of the Neo4j server private key
#   TLS_CA_CERT_CONTENT      — PEM content of the CA certificate
#
# Certificate layout:
#   $NEO4J_HOME/certificates/
#     https/
#       public.crt      ← server certificate
#       private.key     ← server private key
#       trusted/
#         ca.pem        ← CA certificate for client verification
#     bolt/
#       public.crt
#       private.key
#       trusted/
#         ca.pem

let neo4j_home = "/var/lib/neo4j"

# ── Step 1: Copy neo4j.conf ───────────────────────────────────────────────────
print "[neo4j-init] Copying neo4j.conf..."
cp /config/neo4j.conf ($neo4j_home | path join "conf/neo4j.conf")
print "[neo4j-init] neo4j.conf copied."

# ── Step 2: Write TLS certificates ───────────────────────────────────────────
print "[neo4j-init] Writing TLS certificates..."

let tls_server_cert = ($env.TLS_SERVER_CERT_CONTENT? | default "")
let tls_server_key  = ($env.TLS_SERVER_KEY_CONTENT?  | default "")
let tls_ca_cert     = ($env.TLS_CA_CERT_CONTENT?     | default "")

if ($tls_server_cert | is-empty) {
    print "[neo4j-init] WARNING: TLS_SERVER_CERT_CONTENT is empty — skipping cert write"
} else {
    for protocol in ["https", "bolt"] {
        let cert_dir    = ($neo4j_home | path join $"certificates/($protocol)")
        let trusted_dir = ($cert_dir | path join "trusted")

        mkdir $trusted_dir

        # Remove stale cert files from previous deployments
        for stale in ["tls.crt", "tls.key", "public.crt", "private.key"] {
            let p = ($cert_dir | path join $stale)
            if ($p | path exists) { rm $p }
        }
        let stale_ca = ($trusted_dir | path join "ca.pem")
        if ($stale_ca | path exists) { rm $stale_ca }

        # Write certs
        $tls_server_cert | save --force ($cert_dir | path join "public.crt")
        $tls_server_key  | save --force ($cert_dir | path join "private.key")
        $tls_ca_cert     | save --force ($trusted_dir | path join "ca.pem")

        print $"[neo4j-init] ($protocol) certificates written to ($cert_dir)"
    }
}

# ── Step 3: Ensure neo4j owns its writable directories ────────────────────────
print "[neo4j-init] Setting ownership on neo4j directories..."

for dir in ["logs", "data", "run", "import", "plugins"] {
    let p = ($neo4j_home | path join $dir)
    mkdir $p
    run-external "chown" "-R" "7474:7474" $p
}

# Also chown the conf and certificates directories we just wrote
run-external "chown" "7474:7474" ($neo4j_home | path join "conf/neo4j.conf")

if not ($tls_server_cert | is-empty) {
    run-external "chown" "-R" "7474:7474" ($neo4j_home | path join "certificates")
}

print "[neo4j-init] Complete."
