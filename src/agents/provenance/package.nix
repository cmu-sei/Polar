
{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:

let
    bin = craneLib.buildPackage (crateArgs // {
        pname = "provenance";
        cargoExtraArgs= "--bin provenance --locked";
        src = workspaceFileset ./provenance;
    });

    image = pkgs.dockerTools.buildImage {
        name = "polar-provenance-agent";
        tag = "latest";
        copyToRoot = commonPaths ++ [bin];
        uid = commonUser.uid;
        gid = commonUser.gid;

        config = {
            User = "${commonUser.uid}:${commonUser.gid}";
            Cmd = [ "provenance" ];
            WorkingDir = "/";
            Env = [ ];
        };
    };

in
{
  inherit bin image;
}
