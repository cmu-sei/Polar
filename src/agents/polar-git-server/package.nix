# src/agents/polar-git-server/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
, nix-container-lib
, inputs
, system
, healthcheckDrv
, ...
}:

let
  # The trees the server commits as the first commit of a repository it finds empty, one directory
  # per repository, taken from infra/seeds.
  seeds = pkgs.runCommand "polar-git-server-seeds" { } ''
    cp -r ${../../../infra/seeds} $out
  '';

  server = craneLib.buildPackage (crateArgs // {
    pname = "polar-git-server";
    cargoExtraArgs = "--bin polar-git-server --locked";
    src = workspaceFileset ./server;
    # The binary's default seed root. Having the path in the binary is also what puts the seeds
    # in the image: the container only links bin/ from its packages, but it carries each one's
    # whole closure, and a store path in the binary is a reference.
    POLAR_GIT_SERVER_BUILT_SEED_ROOT = "${seeds}";
  });

  container = nix-container-lib.lib.${system}.mkContainer {
    inherit system pkgs;
    inputs = inputs // {
      polar-healthcheck = {
        packages.${system}.default = healthcheckDrv;
      };
    };
    configNixPath = ./container.nix;
    # The server runs git (upload-pack, receive-pack) for every request.
    extraDerivations = [ server pkgs.git ];
  };
in
{
  inherit server seeds;
  image = container.image;
}
