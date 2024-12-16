{ pkgs }:

# Download the tls-gen tool from GitHub,
# run make to generate a CA certificate, a server certificate, and a client certificate.

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
  
  nativeBuildInputs = [ pkgs.cmake pkgs.openssl pkgs.hostname];

  #Prevent usage of cmakeConfigurePhase
  # CAUTION: We probably won't ever change this, removing it creates an error on x86_64 linux that prevents the CMAKELists.txt file from being found
  # REFERENCE:https://stackoverflow.com/questions/70513330/mkderivation-use-cmake-in-buildphase
  # REFERENCE: https://nixos.org/manual/nixpkgs/stable/#dont-use-cmake-configure
  dontUseCmakeConfigure = true;
  
  #make ca certs with basic profile
  buildPhase = ''
    cd basic
    # pass a private key password using the PASSWORD variable if needed
    make CN=rabbitmq 
  '';

  #copy out our files
  installPhase = ''
  mkdir -p $out/certs
  cp result/client_rabbitmq_certificate.pem $out/certs/client_rabbitmq_certificate.pem
  cp result/client_rabbitmq_key.pem         $out/certs/client_rabbitmq_key.pem
  cp result/server_rabbitmq_certificate.pem $out/certs/server_rabbitmq_certificate.pem
  cp result/server_rabbitmq_key.pem         $out/certs/server_rabbitmq_key.pem   
  '';
}
