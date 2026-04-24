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

    # ── Step 1: Generate CA + Cassini server/client certs via tls-gen ────────
    make CN=cassini SERVER_ALT_NAME=cassini

    # ── Step 2: Generate a clean Neo4j CA (no tls-gen, no special chars) ─────
    mkdir -p neo4j_ca/certs neo4j_ca/private
    touch neo4j_ca/index.txt
    echo "01" > neo4j_ca/serial

    openssl req -x509 \
      -newkey rsa:4096 \
      -days 3650 \
      -nodes \
      -keyout neo4j_ca/private/cakey.pem \
      -out neo4j_ca/cacert.pem \
      -subj "/CN=Polar-Neo4j-CA/O=Polar"

    # ── Step 3: Generate Neo4j server cert signed by the Neo4j CA ─────────────
    mkdir -p server_neo4j

    cat > neo4j-openssl.cnf << 'EOF'
[ ca ]
default_ca = neo4j_ca

[ neo4j_ca ]
dir               = neo4j_ca
certificate       = $dir/cacert.pem
database          = $dir/index.txt
new_certs_dir     = $dir/certs
private_key       = $dir/private/cakey.pem
serial            = $dir/serial
default_crl_days  = 7
default_days      = 3650
default_md        = sha256
policy            = neo4j_ca_policy
x509_extensions   = certificate_extensions

[ neo4j_ca_policy ]
commonName              = supplied
organizationName        = optional
stateOrProvinceName     = optional
countryName             = optional

[ certificate_extensions ]
basicConstraints = CA:false

[ req ]
default_bits       = 4096
default_md         = sha256
prompt             = no
distinguished_name = req_distinguished_name

[ req_distinguished_name ]
CN = hostname

[ client_extensions ]
basicConstraints     = CA:false
keyUsage             = digitalSignature,keyEncipherment
extendedKeyUsage     = clientAuth
subjectAltName       = @neo4j_client_alt_names
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer

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

    openssl genpkey \
      -algorithm RSA \
      -outform PEM \
      -out server_neo4j/key.pem \
      -pkeyopt rsa_keygen_bits:4096

    openssl req \
      -config neo4j-openssl.cnf \
      -new \
      -key server_neo4j/key.pem \
      -out server_neo4j/req.pem \
      -outform PEM \
      -subj "/CN=neo4j/O=server" \
      -nodes

    openssl ca \
      -config neo4j-openssl.cnf \
      -days 3650 \
      -cert neo4j_ca/cacert.pem \
      -keyfile neo4j_ca/private/cakey.pem \
      -in server_neo4j/req.pem \
      -out server_neo4j/cert.pem \
      -outdir neo4j_ca/certs \
      -notext \
      -batch \
      -extensions server_extensions

    # ── Step 4: Generate Neo4j client cert signed by the Neo4j CA ────────────
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
      -subj "/CN=neo4j/O=client" \
      -nodes

    openssl ca \
      -config neo4j-openssl.cnf \
      -days 3650 \
      -cert neo4j_ca/cacert.pem \
      -keyfile neo4j_ca/private/cakey.pem \
      -in client_neo4j/req.pem \
      -out client_neo4j/cert.pem \
      -outdir neo4j_ca/certs \
      -notext \
      -batch \
      -extensions client_extensions
  '';

  installPhase = ''
    # Debug: what's actually here?
    ls -la
    ls -la basic/ || echo "no basic/"
    ls -la result/ || echo "no result/"
    
    # Try using the actual structure — if the build generated things in-place
    # under the source tree, the paths from the build log should work
    cd basic 2>/dev/null || cd tls-gen/basic 2>/dev/null || true
    
    # ── Cassini certs ─────────────────────────────────────────────────────────
    mkdir -p $out/ca_certificates
    mkdir -p $out/server
    mkdir -p $out/client
    cp result/ca*     $out/ca_certificates/ 2>/dev/null || \
    cp basic/result/ca* $out/ca_certificates/ 2>/dev/null || \
    cp */result/ca* $out/ca_certificates/
    
    cp result/client* $out/client/ 2>/dev/null || \
    cp basic/result/client* $out/client/
    
    cp result/server* $out/server/ 2>/dev/null || \
    cp basic/result/server* $out/server/

    # ── Neo4j certs ──────────────────────────────────────────────────────────
    mkdir -p $out/neo4j
    cp server_neo4j/cert.pem     $out/neo4j/server_neo4j_certificate.pem 2>/dev/null || \
    cp */server_neo4j/cert.pem   $out/neo4j/server_neo4j_certificate.pem
    cp server_neo4j/key.pem      $out/neo4j/server_neo4j_key.pem 2>/dev/null || \
    cp */server_neo4j/key.pem    $out/neo4j/server_neo4j_key.pem
    cp client_neo4j/cert.pem     $out/neo4j/client_neo4j_certificate.pem 2>/dev/null || \
    cp */client_neo4j/cert.pem   $out/neo4j/client_neo4j_certificate.pem
    cp client_neo4j/key.pem      $out/neo4j/client_neo4j_key.pem 2>/dev/null || \
    cp */client_neo4j/key.pem    $out/neo4j/client_neo4j_key.pem
    cp neo4j_ca/cacert.pem       $out/neo4j/ca_certificate.pem 2>/dev/null || \
    cp */neo4j_ca/cacert.pem     $out/neo4j/ca_certificate.pem
  '';
}
