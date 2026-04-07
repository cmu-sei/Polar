{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:

let
    extraCommands = ''
      mkdir -p etc
      printf 'polar:x:1000:1000::/home/polar:/bin/bash\n' > etc/passwd
      printf 'polar:x:1000:\n' > etc/group
      printf 'polar:!x:::::::\n' > etc/shadow
    '';

    linkerBin = craneLib.buildPackage (crateArgs // {
        pname = "provenance-linker";
        cargoExtraArgs= "--bin provenance-linker --locked";
        src = workspaceFileset ./provenance;
    });

    linkerImage = pkgs.dockerTools.buildImage {
        inherit extraCommands;
        name = "provenance-linker-agent";
        tag = "latest";
        contents = commonPaths ++ [linkerBin];

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
      name          = "resolver.json";
      destination   = "/resolver.json";
      text          = builtins.readFile ./resolver/resolver.json;
    };

    resolverImage = pkgs.dockerTools.buildLayeredImage {
        inherit extraCommands;
        name = "provenance-resolver-agent";
        tag = "latest";
        contents = commonPaths ++ [
          resolverBin
          pkgs.cacert
          resolverConfig
        ];
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
