# An S3-compatible object store for build-orchestrator's logs, for a cluster that has none: the Versity S3 gateway over a
# directory, from nixpkgs. (MinIO's community edition is no longer maintained and nixpkgs marks it insecure.)
# On a cloud, a bucket takes its place and this is not deployed.
#
# The gateway wants its directories to exist, and a new volume has none, so the entrypoint makes them. Its
# arguments are the deployment's (library `types/build-orchestrator.dhall`), and its keys are the variables
# ROOT_ACCESS_KEY and ROOT_SECRET_KEY.
{ pkgs }:
let
  entrypoint = pkgs.writeShellApplication {
    name = "build-orchestrator-object-store";
    runtimeInputs = [ pkgs.coreutils pkgs.versitygw ];
    text = ''
      mkdir -p /data/buckets /data/sidecar
      exec versitygw "$@"
    '';
  };
in
pkgs.dockerTools.buildLayeredImage {
  name = "build-orchestrator-object-store";
  tag = "latest";
  contents = [ entrypoint pkgs.cacert ];
  config = {
    Entrypoint = [ "/bin/build-orchestrator-object-store" ];
    Env = [ "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" "PATH=/bin" ];
    ExposedPorts = { "9000/tcp" = { }; };
  };
}
