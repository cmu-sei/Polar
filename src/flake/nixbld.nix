#
# A base environment definition for configuring a filesystem with a static number of nixbld users.
# This enables nix to run in a containeriszed env, but limtis its build capacity to only
# TODO: Configure filesystem for a single-user nix installation.
{ pkgs }:

let

    # define a fixed number of build users;
   buildUserCount = 10;

   nixbldUsers = builtins.genList (n: {
     name = "nixbld${toString (n + 1)}";
     uid = 30000 + n;
     gid = 30000;
   }) buildUserCount;


   # Join user entries into each file format
   passwdEntries = builtins.concatStringsSep "\n" (
     ["root:x:0:0::/root:${pkgs.runtimeShell}"]
     ++ map (u: "${u.name}:x:${toString u.uid}:${toString u.gid}::/var/empty:/sbin/nologin") nixbldUsers
   );

   nixldGroupEntry = "nixbld:x:30000:" + (builtins.concatStringsSep "," (map (u: u.name) nixbldUsers));
   groupEntries = builtins.concatStringsSep "\n" (
     ["root:x:0:" nixldGroupEntry]
   );
   nixbldShadow = "nixbld:!:" + (builtins.concatStringsSep "," (map (u: u.name) nixbldUsers)) + ":";
   gshadowEntries = builtins.concatStringsSep "\n" (
     ["root:x::" nixbldShadow]
   );

   shadowEntries = "root:!x:::::::"; # unchanged

   shellsFile = ''
     /bin/sh
     /bin/bash
     /bin/fish
   '';

   osRelease = ''
     NAME="NixOS"
     ID=nixos
     VERSION="unstable"
     VERSION_CODENAME=unstable
     PRETTY_NAME="NixOS (unstable)"
     HOME_URL="https://nixos.org/"
     SUPPORT_URL="https://nixos.org/nixos/manual/"
     BUG_REPORT_URL="https://github.com/NixOS/nixpkgs/issues"
   '';



   baseInfo = with pkgs; [
     (writeTextDir "etc/shadow" shadowEntries)
     (writeTextDir "etc/passwd" passwdEntries)
     (writeTextDir "etc/group" groupEntries)
     (writeTextDir "etc/gshadow" gshadowEntries)
     (writeTextDir "etc/shells" shellsFile)
     (writeTextDir "etc/os-release" osRelease)
   ];

  in baseInfo
