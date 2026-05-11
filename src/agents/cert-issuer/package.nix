# src/agents/cert-issuer/package.nix
{ pkgs
, craneLib
, crateArgs
, workspaceFileset
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
    pname = "cert-client";
    cargoExtraArgs = "--bin cert-client --locked";
    src = workspaceFileset ./cert-issuer/init;
    doCheck = false;
  });

  setupBin = craneLib.buildPackage (crateArgs // {
    pname = "cert-issuer-setup";
    cargoExtraArgs = "--bin cert-issuer-setup --locked";
    src = workspaceFileset ./cert-issuer/init;
    doCheck = false;
  });


  # TODO: Integrate this image with the larger container library
  polarUid = "10000";
  polarGid = "10000";

  passwdFile = pkgs.writeText "passwd" ''
    root:x:0:0:root:/root:/sbin/nologin
    polar:x:${polarUid}:${polarGid}:Polar cert issuer:/home/polar:/sbin/nologin
  '';

  groupFile = pkgs.writeText "group" ''
    root:x:0:
    polar:x:${polarGid}:
  '';

  serverImage = pkgs.dockerTools.buildLayeredImage {
    name = "cert-issuer";
    tag  = "latest";

    contents = [
      server
      pkgs.iana-etc
      pkgs.cacert
      pkgs.busybox
    ];

    # fakeRootCommands runs as root during image build, letting us
    # create directories with correct ownership before the image is
    # sealed. The polar user can then write into its own workspace
    # at runtime without any privilege escalation.
    fakeRootCommands = ''
      # Identity files
      mkdir -p ./etc
      cp ${passwdFile} ./etc/passwd
      cp ${groupFile}  ./etc/group

      # polar's home directory
      mkdir -p ./home/polar
      chown ${polarUid}:${polarGid} ./home/polar

      # CA materials workspace. The cert issuer writes ca.crt and
      # ca.key here on first run. In production this path is
      # shadowed by the PVC mount in the pod spec — the image
      # directory is a fallback for local testing only.
      # Located under the user's home rather than /etc so the
      # polar user has write access without any root involvement.
      mkdir -p ./home/polar/ca
      chown ${polarUid}:${polarGid} ./home/polar/ca

      # Config directory is read-only at runtime (ConfigMap mount
      # in the pod spec), but must exist in the image so the
      # process can stat the path before the mount lands.
      mkdir -p ./etc/cert-issuer
      chown ${polarUid}:${polarGid} ./etc/cert-issuer
    '';

    enableFakechroot = true;

    config = {
      Entrypoint = [ "${server}/bin/cert-issuer" ];
      Cmd        = [];
      User       = "${polarUid}:${polarGid}";
      WorkingDir = "/home/polar";
      ExposedPorts = { "8443/tcp" = {}; };
      Env = [
        "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "RUST_LOG=info,cert_issuer=debug"
        # Default CA paths pointing at the user-owned workspace.
        # The Dhall config and pod spec ConfigMap override these
        # in cluster — these defaults make local testing with
        # cert-issuer-setup work without any extra configuration.
        "CERT_ISSUER_CA_CERT_PATH=/home/polar/ca/ca.crt"
        "CERT_ISSUER_CA_KEY_PATH=/home/polar/ca/ca.key"
      ];
      Labels = {
        "org.opencontainers.image.title"       = "cert-issuer";
        "org.opencontainers.image.description" = "Polar internal CA and OIDC-backed cert issuer";
      };
    };
  };

in
{
  inherit server client setupBin serverImage;
}
