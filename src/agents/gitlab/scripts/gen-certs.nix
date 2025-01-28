{ pkgs }:

# Download the tls-gen tool from GitHub,
# run make to generate a CA certificate, a server certificate, and a client certificate.
# REFERENCE: https://nixos.org/manual/nixpkgs/stable/#sec-using-stdenv

pkgs.stdenv.mkDerivation {
  pname = "tls-gen-certificates";
  version = "1.0.0";

  src = builtins.fetchGit {
    url = "https://github.com/rabbitmq/tls-gen.git";
    name = "tls-gen";
    rev = "efb3766277d99c6b8512f226351c7a62f492ef3f";
  };
  
  #set build inputs, tls-gen requires python and cmake
  buildInputs = [ pkgs.python312 ];
  
  # TODO: use the FIPS compliant SSL package
  # REFERENCE: https://github.com/MaxfieldKassel/nix-flake-openssl-fips
  nativeBuildInputs = [ pkgs.cmake pkgs.openssl pkgs.hostname];

  #Prevent usage of cmakeConfigurePhase
  # CAUTION: We probably won't ever change this, removing it creates an error on x86_64 linux that prevents the CMAKELists.txt file from being found
  # REFERENCE: https://stackoverflow.com/questions/70513330/mkderivation-use-cmake-in-buildphase
  # REFERENCE: https://nixos.org/manual/nixpkgs/stable/#dont-use-cmake-configure
  dontUseCmakeConfigure = true;
  
  #make ca certs with basic profile
  buildPhase = ''
    cd basic
    # pass a private key password using the PASSWORD variable if needed
    # TODO: Add a password, managed as a secret?
    make CN=polar 
  '';

  #copy out our files
  installPhase = ''

  #use openssl to create p12 file
  openssl pkcs12 -legacy -export -inkey result/client_rabbitmq_key.pem -in result/client_rabbitmq_certificate.pem -out client_rabbitmq.p12 -passout pass:""
  
  mkdir -p $out/ca_certificates
  mkdir -p $out/client
  mkdir -p $out/server

  cp result/ca* $out/ca_certificates
  cp result/client* $out/client
  cp result/server* $out/server
  '';
}
