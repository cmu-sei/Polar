# src/agents/cassini/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, ...
}:
let

  server = craneLib.buildPackage (crateArgs // {
    pname = "cert-issuer";
    cargoExtraArgs = "--bin cert-issuer --locked";
    src = workspaceFileset ./cert-issuer/agent;
    doCheck = false;
  });

  client = craneLib.buildPackage (crateArgs // {
    pname = "cert-issuer";
    cargoExtraArgs = "--bin cert-issuer-init --locked";
    src = workspaceFileset ./cert-issuer/init;
    doCheck = false;
  });


  setupBin = craneLib.buildPackage (crateArgs // {
    pname = "cert-issuer-setup";
    cargoExtraArgs = "--bin cert-issuer-setup --locked";
    src = workspaceFileset ./cert-issuer/init;
    doCheck = false;
  });

  # TODO: Render and build the container image for this service
  # certIssuerContainer = nix-container-lib.lib.${system}.mkContainer {
  #   inherit system pkgs inputs;
  #   configNixPath    = ./container-cert.nix;
  #   extraDerivations = [ cassini ];
  # };

in
{
  inherit server client setupBin;
}
