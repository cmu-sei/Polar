
{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:

let

    linkerBin = craneLib.buildPackage (crateArgs // {
        pname = "provenance-linker";
        cargoExtraArgs= "--bin provenance-linker --locked";
        src = workspaceFileset ./provenance;
    });

    linkerImage = pkgs.dockerTools.buildImage {
        name = "provenance-linker-agent";
        tag = "latest";
        copyToRoot = commonPaths ++ [linkerBin];
        uid = commonUser.uid;
        gid = commonUser.gid;

        config = {
            User = "${commonUser.uid}:${commonUser.gid}";
            Cmd = [ "provenance-linker" ];
            WorkingDir = "/";
            Env = [ ];
        };
    };

    resolverBin = craneLib.buildPackage (crateArgs // {
        pname = "provenance-resolver";
        cargoExtraArgs= "--bin provenance-resolver --locked";
        src = workspaceFileset ./provenance;
    });

    resolverConfig = pkgs.writeTextFile {
      name          = "resolver.dhall";
      destination   = "/resolver.dhall";
      text          = builtins.readFile ./resolver/resolver.dhall;
    };

    resolverImage = pkgs.dockerTools.buildImage {
        name = "provenance-resolver-agent";
        tag = "latest";
        copyToRoot = commonPaths ++ [
          resolverBin
          pkgs.cacert # this one needs a default trust store, so we'll include one
          resolverConfig
        ];
        uid = commonUser.uid;
        gid = commonUser.gid;

        config = {
            User = "${commonUser.uid}:${commonUser.gid}";
            Cmd = [ "provenance-resolver" ];
            WorkingDir = "/";
            Env = [ ];
        };
    };


in
{
  inherit linkerBin resolverBin linkerImage resolverImage;
}
