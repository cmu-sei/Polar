#!/usr/bin/env nu

# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# infra/layers/2-services/neo4j/setup-neo4j.nu
#
# Neo4j init container script. Runs as polar-nu-init:latest.
# Mounted at /scripts/init.nu in the init container pod spec.
#
# Responsibilities:
#   1. Copy neo4j.conf from the ConfigMap mount to $NEO4J_HOME/conf/
#      (Neo4j requires the config file to be writable; ConfigMap mounts
#      are read-only in Kubernetes.)
#   2. Write TLS certificates from the polar-init emptyDir mount into
#      the directory structure Neo4j expects for mTLS on both HTTPS and Bolt.
#   3. Ensure neo4j (uid/gid 7474) owns all writable directories.
#
# Certificate source:
#   /etc/neo4j-tls/cert.pem  — server certificate (written by polar-init)
#   /etc/neo4j-tls/key.pem   — server private key  (written by polar-init)
#   /etc/neo4j-tls/ca.pem    — CA certificate       (written by polar-init)
#
# Certificate layout written by this script:
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

let neo4j_home  = "/var/lib/neo4j"
let neo4j_tls   = "/etc/neo4j-tls"

# ── Step 1: Copy neo4j.conf ───────────────────────────────────────────────────
print "[neo4j-init] Copying neo4j.conf..."
cp /config/neo4j.conf ($neo4j_home | path join "conf/neo4j.conf")
print "[neo4j-init] neo4j.conf copied."

# ── Step 2: Write TLS certificates ───────────────────────────────────────────
print "[neo4j-init] Writing TLS certificates..."

let tls_server_cert = (open ($neo4j_tls | path join "cert.pem"))
let tls_server_key  = (open ($neo4j_tls | path join "key.pem"))
let tls_ca_cert     = (open ($neo4j_tls | path join "ca.pem"))

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

# ── Step 3: Ensure neo4j owns its writable directories ────────────────────────
print "[neo4j-init] Setting ownership on neo4j directories..."

for dir in ["logs", "data", "run", "import", "plugins"] {
    let p = ($neo4j_home | path join $dir)
    mkdir $p
    run-external "chown" "-R" "7474:7474" $p
}

# Also chown the conf and certificates directories we just wrote
run-external "chown" "7474:7474" ($neo4j_home | path join "conf/neo4j.conf")
run-external "chown" "-R" "7474:7474" ($neo4j_home | path join "certificates")

print "[neo4j-init] Complete."
