#!/bin/sh
# setup-neo4j.sh
#
# Init container script for Neo4j. Runs as the neo4j user (uid/gid 7474).
#
# Responsibilities:
#   1. Copy neo4j.conf from the ConfigMap mount to $NEO4J_HOME/conf/
#      (Neo4j requires the config file to be writable, but ConfigMap mounts
#      are read-only in Kubernetes.)
#   2. Write TLS certificates from environment variables into the directory
#      structure Neo4j expects for mTLS on both HTTPS and Bolt.
#
# Environment variables required for TLS:
#   TLS_SERVER_CERT_CONTENT  — PEM content of the Neo4j server certificate
#   TLS_SERVER_KEY_CONTENT   — PEM content of the Neo4j server private key
#   TLS_CA_CERT_CONTENT      — PEM content of the CA certificate (used to
#                              verify client certificates)
#
# Certificate layout written by this script:
#   $NEO4J_HOME/certificates/
#     https/
#       public.crt      ← server certificate (Neo4j expects this exact name)
#       private.key     ← server private key (Neo4j expects this exact name)
#       trusted/
#         ca.crt        ← CA certificate for client verification
#     bolt/
#       public.crt      ← server certificate (Neo4j expects this exact name)
#       private.key     ← server private key (Neo4j expects this exact name)
#       trusted/
#         ca.crt        ← CA certificate for client verification

set -e

NEO4J_HOME=/var/lib/neo4j

# ── Step 1: Copy neo4j.conf ───────────────────────────────────────────────────
echo "[INIT] Copying neo4j.conf..."
cp /config/neo4j.conf $NEO4J_HOME/conf/neo4j.conf
chown 7474:7474 $NEO4J_HOME/conf/neo4j.conf
echo "[INIT] neo4j.conf copied."

# ── Step 2: Write TLS certificates ───────────────────────────────────────────
echo "[INIT] Writing TLS certificates..."

for PROTOCOL in https bolt; do
    CERT_DIR=$NEO4J_HOME/certificates/$PROTOCOL

    mkdir -p $CERT_DIR/trusted

    # Remove any stale cert files from previous deployments
    rm -f $CERT_DIR/tls.crt $CERT_DIR/tls.key $CERT_DIR/public.crt $CERT_DIR/private.key
    #rm -f $CERT_DIR/trusted/ca.crt
    rm -f $CERT_DIR/trusted/ca.pem

    # Write server cert and key
    printf '%s' "$TLS_SERVER_CERT_CONTENT" > $CERT_DIR/public.crt
    printf '%s' "$TLS_SERVER_KEY_CONTENT"  > $CERT_DIR/private.key

    # Write CA cert into trusted/ so Neo4j can verify client certificates
    #printf '%s' "$TLS_CA_CERT_CONTENT" > $CERT_DIR/trusted/ca.crt
    printf '%s' "$TLS_CA_CERT_CONTENT" > $CERT_DIR/trusted/ca.pem

    # Neo4j runs as uid/gid 7474 — ensure it can read the certs
    chmod 640 $CERT_DIR/public.crt
    chmod 600 $CERT_DIR/private.key
    #chmod 640 $CERT_DIR/trusted/ca.crt
    chmod 640 $CERT_DIR/trusted/ca.pem
    chown 7474:7474 $CERT_DIR/public.crt
    chown 7474:7474 $CERT_DIR/private.key
    #chown 7474:7474 $CERT_DIR/trusted/ca.crt
    chown 7474:7474 $CERT_DIR/trusted/ca.pem
    chown 7474:7474 $CERT_DIR/trusted
    chown 7474:7474 $CERT_DIR

    echo "[INIT] $PROTOCOL certificates written to $CERT_DIR"
done

echo "[INIT] Done."
