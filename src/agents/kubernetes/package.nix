
{ pkgs,
  commonPaths,
  craneLib,
  crateArgs,
  workspaceFileset,
  cargoArtifacts,
  commonUser,
}:


let
    observer = craneLib.buildPackage (crateArgs // {
        pname = "kube-observer";
        cargoExtraArgs= "--bin kube-observer --locked";
        src = workspaceFileset ./kubernetes/observe;
    });

    consumer = craneLib.buildPackage (crateArgs // {
        pname = "kube-consumer";
        cargoExtraArgs= "--bin kube-observer --locked";
        src = workspaceFileset ./kubernetes/consume;
    });

    observerImage = pkgs.dockerTools.buildImage {
        name = "polar-kube-observer";
        tag = "latest";
        copyToRoot = commonPaths ++ [observer];
        uid = commonUser.uid;
        gid = commonUser.gid;

        config = {
            User = "${commonUser.uid}:${commonUser.gid}";
            Cmd = [ "kube-observer" ];
            WorkingDir = "/";
            Env = [ ];
        };
    };

    consumerImage = pkgs.dockerTools.buildImage {
        name = "polar-kube-consumer";
        tag = "latest";
        copyToRoot = commonPaths ++ [consumer];
        uid = commonUser.uid;
        gid = commonUser.gid;

        config = {
            User = "${commonUser.uid}:${commonUser.gid}";
            Cmd = [ "kube-consumer" ];
            WorkingDir = "/";
            Env = [ ];
        };
    };
in
{
  inherit observer consumer observerImage consumerImage;
}
