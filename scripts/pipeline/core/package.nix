# Deliberately boring package file to gather all files here and return them as a fileset so we can export them as a flake output
{ runCommand }:

runCommand "polar-core" {} ''
  mkdir -p $out

  cp ${./mod.nu} $out/mod.nu
  cp ${./cargo.nu} $out/cargo.nu
  cp ${./cassini.nu} $out/cassini.nu
  cp ${./dhall.nu} $out/dhall.nu
  cp ${./events.nu} $out/events.nu
  cp ${./hashing.nu} $out/hashing.nu
  cp ${./logging.nu} $out/logging.nu
  cp ${./oci.nu} $out/oci.nu
  cp ${./sbom.nu} $out/sbom.nu
  cp ${./scanning.nu} $out/scanning.nu
  cp ${./state.nu} $out/state.nu
''
