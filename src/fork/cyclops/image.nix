# The Cyclops image, from binaries built in the fork's development shell.
#
# This is the local route. The fork builds its agents' images with crane from its own workspace; this
# workspace lives outside it (see Cargo.toml), so a crane build would need both source trees as one input.
# Until that exists the binaries are built with cargo in the fork's dev shell (README) and wrapped here,
# patched to the nixpkgs the fork pins. The image has what the operator needs of an agent: the binary, and
# polar-healthcheck, which the liveness probe runs.
{ pkgs, orchestrator, healthcheck }:
let
  binaries = pkgs.runCommand "cyclops-binaries"
    {
      nativeBuildInputs = [ pkgs.autoPatchelfHook ];
      buildInputs = [ pkgs.stdenv.cc.cc.lib ];
    }
    ''
      install -Dm755 ${orchestrator} $out/bin/cyclops
      install -Dm755 ${healthcheck} $out/bin/polar-healthcheck
    '';
in
pkgs.dockerTools.buildLayeredImage {
  name = "cyclops";
  tag = "latest";
  contents = [ binaries pkgs.cacert ];
  config = {
    Cmd = [ "/bin/cyclops" ];
    Env = [ "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt" "PATH=/bin" ];
  };
}
