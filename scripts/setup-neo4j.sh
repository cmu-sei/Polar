#!/bin/sh
# setup-neo4j.sh
#
# Init container script for Neo4j. Runs as root (uid 0) so it can set
# ownership on directories before the main neo4j container starts as uid 7474.
#
# Runs after polar-nu-init has written cert.pem / key.pem / ca.pem to the
# shared cert emptyDir. Reads cert material from files rather than environment
# variables.
#
# Responsibilities:
#   1. Verify cert files written by polar-nu-init are present.
#   2. Copy neo4j.conf from the ConfigMap mount to $NEO4J_HOME/conf/
#   3. Lay out cert files into the directory structure Neo4j expects
#      for mTLS on both HTTPS and Bolt protocols.
#   4. Ensure neo4j (uid/gid 7474) owns all writable directories.
#
# Required env vars:
#   POLAR_CERT_DIR  — directory where polar-nu-init wrote the cert bundle
#                     defaults to /var/lib/neo4j/certificates if not set
#
# Certificate layout written by this script:
#   $NEO4J_HOME/certificates/
#     https/
#       public.crt      ← server certificate
#       private.key     ← server private key
#       trusted/
#         ca.pem        ← CA certificate for client verification
#     bolt/
#       (same layout)

set -eu

NEO4J_HOME=/var/lib/neo4j
POLAR_CERT_DIR="${POLAR_CERT_DIR:-/var/lib/neo4j/certificates}"

SRC_CERT="$POLAR_CERT_DIR/cert.pem"
SRC_KEY="$POLAR_CERT_DIR/key.pem"
SRC_CA="$POLAR_CERT_DIR/ca.pem"

# ── Preflight: verify polar-nu-init wrote the cert bundle ─────────────────────

echo "[INIT] Verifying cert bundle from polar-nu-init..."

for f in "$SRC_CERT" "$SRC_KEY" "$SRC_CA"; do
    if [ ! -f "$f" ]; then
        echo "[INIT] ERROR: expected cert file not found: $f"
        echo "[INIT] polar-nu-init must complete successfully before this container runs"
        exit 1
    fi
done

echo "[INIT] Cert bundle verified."

# ── Step 1: Copy neo4j.conf ───────────────────────────────────────────────────

echo "[INIT] Copying neo4j.conf..."
cp /config/neo4j.conf "$NEO4J_HOME/conf/neo4j.conf"
chown 7474:7474 "$NEO4J_HOME/conf/neo4j.conf"
echo "[INIT] neo4j.conf copied."

# ── Step 2: Lay out TLS certificates ─────────────────────────────────────────

echo "[INIT] Writing TLS certificate layout..."

for PROTOCOL in https bolt; do
    CERT_DIR="$NEO4J_HOME/certificates/$PROTOCOL"
    mkdir -p "$CERT_DIR/trusted"

    # Remove stale cert files from previous runs
    rm -f \
        "$CERT_DIR/tls.crt" \
        "$CERT_DIR/tls.key" \
        "$CERT_DIR/public.crt" \
        "$CERT_DIR/private.key" \
        "$CERT_DIR/trusted/ca.pem"

    # Copy from polar-nu-init output
    cp "$SRC_CERT" "$CERT_DIR/public.crt"
    cp "$SRC_KEY"  "$CERT_DIR/private.key"
    cp "$SRC_CA"   "$CERT_DIR/trusted/ca.pem"

    chmod 640 "$CERT_DIR/public.crt"
    chmod 600 "$CERT_DIR/private.key"
    chmod 640 "$CERT_DIR/trusted/ca.pem"
    chown -R 7474:7474 "$CERT_DIR"

    echo "[INIT] $PROTOCOL certificates written to $CERT_DIR"
done

# ── Step 3: Fix ownership on Neo4j directories ────────────────────────────────

echo "[INIT] Setting ownership on neo4j directories..."

mkdir -p \
    "$NEO4J_HOME/logs"    \
    "$NEO4J_HOME/data"    \
    "$NEO4J_HOME/run"     \
    "$NEO4J_HOME/import"  \
    "$NEO4J_HOME/plugins"

chown -R 7474:7474 \
    "$NEO4J_HOME/logs"    \
    "$NEO4J_HOME/data"    \
    "$NEO4J_HOME/run"     \
    "$NEO4J_HOME/import"  \
    "$NEO4J_HOME/plugins"

echo "[INIT] Complete."
