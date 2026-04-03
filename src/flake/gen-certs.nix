{ pkgs }:
# Generates TLS certificates for all Polar services using the RabbitMQ tls-gen tool.
#
# Two certificate sets are produced, both signed by the same CA:
#
#   Cassini (message broker):
#     $out/ca_certificates/ca_certificate.pem  (and key)
#     $out/server/server_cassini_certificate.pem  (and key)
#     $out/client/client_cassini_certificate.pem  (and key)
#
#   Neo4j (graph database):
#     $out/neo4j/server_neo4j_certificate.pem  (and key)
#     $out/neo4j/client_neo4j_certificate.pem  (and key)
#     (shares the same CA as Cassini)
#
# The Neo4j server cert includes SANs for all expected in-cluster hostnames.
#
# REFERENCE: https://nixos.org/manual/nixpkgs/stable/#sec-using-stdenv
# REFERENCE: https://github.com/rabbitmq/tls-gen

pkgs.stdenv.mkDerivation {
  pname = "tls-gen-certificates";
  version = "1.0.0";

  src = builtins.fetchGit {
    url = "https://github.com/rabbitmq/tls-gen.git";
    name = "tls-gen";
    rev = "efb3766277d99c6b8512f226351c7a62f492ef3f";
    ref = "HEAD";
  };

  buildInputs      = [ pkgs.python312 ];
  nativeBuildInputs = [ pkgs.cmake pkgs.openssl pkgs.hostname ];

  # Prevent CMake from trying to run its own configure phase.
  # REFERENCE: https://nixos.org/manual/nixpkgs/stable/#dont-use-cmake-configure
  # REFERENCE: https://stackoverflow.com/questions/70513330
  dontUseCmakeConfigure = true;

  buildPhase = ''
    cd basic

    # ── Step 1: Generate CA + Cassini server/client certs ────────────────────
    make CN=cassini SERVER_ALT_NAME=cassini

    # ── Step 2: Generate Neo4j server cert signed by the same CA ─────────────
    #
    # tls-gen only supports a single SERVER_ALT_NAME make variable, which maps
    # to DNS.2 in the cnf. Neo4j needs several SANs for in-cluster routing, so
    # we patch the openssl.cnf to add them and sign the cert manually.

    # Write a custom cnf with all required Neo4j SANs
    cat > neo4j-openssl.cnf << 'EOF'
common_name = neo4j
client_alt_name = neo4j
server_alt_name = neo4j

[ ca ]
default_ca = test_root_ca

[ test_root_ca ]
root_ca_dir   = testca
certificate   = $root_ca_dir/cacert.pem
database      = $root_ca_dir/index.txt
new_certs_dir = $root_ca_dir/certs
private_key   = $root_ca_dir/private/cakey.pem
serial        = $root_ca_dir/serial
default_crl_days = 7
default_days     = 3650
default_md       = sha256
policy           = test_root_ca_policy
x509_extensions  = certificate_extensions

[ test_root_ca_policy ]
commonName          = supplied
stateOrProvinceName = optional
countryName         = optional
emailAddress        = optional
organizationName    = optional
organizationalUnitName = optional
domainComponent     = optional

[ certificate_extensions ]
basicConstraints = CA:false

[ req ]
default_bits       = 4096
default_md         = sha256
prompt             = yes
distinguished_name = root_ca_distinguished_name
x509_extensions    = root_ca_extensions

[ root_ca_distinguished_name ]
commonName = hostname

[ root_ca_extensions ]
basicConstraints       = critical,CA:true
keyUsage               = keyCertSign, cRLSign
subjectKeyIdentifier   = hash
authorityKeyIdentifier = keyid:always,issuer

[ client_extensions ]
basicConstraints     = CA:false
keyUsage             = digitalSignature,keyEncipherment
extendedKeyUsage     = clientAuth
subjectAltName       = @neo4j_client_alt_names

[ server_extensions ]
basicConstraints       = CA:false
keyUsage               = digitalSignature,keyEncipherment
extendedKeyUsage       = serverAuth
subjectAltName         = @neo4j_server_alt_names
subjectKeyIdentifier   = hash
authorityKeyIdentifier = keyid,issuer

[ neo4j_client_alt_names ]
DNS.1 = neo4j
DNS.2 = localhost

[ neo4j_server_alt_names ]
DNS.1 = neo4j
DNS.2 = polar-neo4j
DNS.3 = polar-db-svc
DNS.4 = polar-db-svc.polar-graph
DNS.5 = polar-db-svc.polar-graph.svc.cluster.local
DNS.6 = localhost
EOF

    # Generate Neo4j server key
    mkdir -p server_neo4j

    openssl genpkey \
      -algorithm RSA \
      -outform PEM \
      -out server_neo4j/key.pem \
      -pkeyopt rsa_keygen_bits:4096

    # Generate CSR for Neo4j server
    openssl req \
      -config neo4j-openssl.cnf \
      -new \
      -key server_neo4j/key.pem \
      -out server_neo4j/req.pem \
      -outform PEM \
      -subj "/CN=neo4j/O=server/L=$$$$/" \
      -nodes

    # Sign Neo4j server cert with the CA generated in step 1
    openssl ca \
      -config neo4j-openssl.cnf \
      -days 3650 \
      -cert testca/cacert.pem \
      -keyfile testca/private/cakey.pem \
      -in server_neo4j/req.pem \
      -out server_neo4j/cert.pem \
      -outdir testca/certs \
      -notext \
      -batch \
      -extensions server_extensions

    # Generate Neo4j client key + cert (for agents connecting to Neo4j with mTLS)
    mkdir -p client_neo4j

    openssl genpkey \
      -algorithm RSA \
      -outform PEM \
      -out client_neo4j/key.pem \
      -pkeyopt rsa_keygen_bits:4096

    openssl req \
      -config neo4j-openssl.cnf \
      -new \
      -key client_neo4j/key.pem \
      -out client_neo4j/req.pem \
      -outform PEM \
      -subj "/CN=neo4j/O=client/L=$$$$/" \
      -nodes

    openssl ca \
      -config neo4j-openssl.cnf \
      -days 3650 \
      -cert testca/cacert.pem \
      -keyfile testca/private/cakey.pem \
      -in client_neo4j/req.pem \
      -out client_neo4j/cert.pem \
      -outdir testca/certs \
      -notext \
      -batch \
      -extensions client_extensions
  '';

  installPhase = ''
    # Nix resets the working directory between phases. The source root is
    # "tls-gen" but Nix may place us in the build dir root. Use an absolute
    # path to find the basic/ directory where our artifacts live.
    BASIC_DIR=$(find /build -maxdepth 3 -name "profile.py" -path "*/basic/*" | head -1 | xargs dirname)
    cd "$BASIC_DIR"

    # ── Cassini certs ─────────────────────────────────────────────────────────
    mkdir -p $out/ca_certificates
    mkdir -p $out/server
    mkdir -p $out/client
    cp result/ca*     $out/ca_certificates/
    cp result/client* $out/client/
    cp result/server* $out/server/

    # ── Neo4j certs ───────────────────────────────────────────────────────────
    mkdir -p $out/neo4j
    cp server_neo4j/cert.pem $out/neo4j/server_neo4j_certificate.pem
    cp server_neo4j/key.pem  $out/neo4j/server_neo4j_key.pem
    cp client_neo4j/cert.pem $out/neo4j/client_neo4j_certificate.pem
    cp client_neo4j/key.pem  $out/neo4j/client_neo4j_key.pem
    # Neo4j shares the CA with Cassini
    cp result/ca_certificate.pem $out/neo4j/ca_certificate.pem
  '';
}
