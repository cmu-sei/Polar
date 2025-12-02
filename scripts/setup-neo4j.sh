#!/bin/sh
# A simple few commands to copy the neo4j configuration to the $NEO4J_HOME directory
# Why? Because neo4j requires that the config be writeable, and we mount it as a configmap,
# Which k8s wants to be read-only
NEO4J_HOME=/var/lib/neo4j

set -e

echo "[INIT] Copying neo4j.conf..."
cp /config/neo4j.conf $NEO4J_HOME/conf/neo4j.conf

# TODO: If we wanted to copy SSL certificates to be *sescure*
# We'd do it this way
# echo "[INIT] Copying certificates..."

# mkdir -p $NEO4J_HOME/certificates/https/trusted
# mkdir -p $NEO4J_HOME/certificates/bolt/trusted

# cp /secrets/tls.key $NEO4J_HOME/certificates/https/tls.key
# cp /secrets/tls.crt $NEO4J_HOME/certificates/https/tls.crt
# cp /secrets/tls.key $NEO4J_HOME/certificates/bolt/tls.key
# cp /secrets/tls.crt $NEO4J_HOME/certificates/bolt/tls.crt
# cp /secrets/tls.crt $NEO4J_HOME/certificates/https/trusted/tls.crt
# cp /secrets/tls.crt $NEO4J_HOME/certificates/bolt/trusted/tls.crt
