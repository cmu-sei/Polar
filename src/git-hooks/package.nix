{ pkgs
, crane
}:

let
  craneLib = (crane.mkLib pkgs).overrideToolchain pkgs.rust-bin.nightly.latest.default;

  commitMsgHook = craneLib.buildPackage ( {
    src = craneLib.cleanCargoSource ./.;
    cargoExtraArgs = "--locked";
    doCheck = false;
  });
in
{
  inherit commitMsgHook;
}
