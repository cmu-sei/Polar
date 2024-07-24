{
  description = "creates a minimal image that runs polar";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils"; # Utility functions for Nix flakes
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable"; # Main Nix package repository
  };

  outputs = { self, flake-utils, nixpkgs, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        polar = pkgs.rustPlatform.buildRustPackage rec {
          pname = "polar";
          version = "f37b0b4";
          # Get Polar and its cargo dependencies. When the version of Polar used is updated,
          # the hashes also need to be changed.
          src = pkgs.fetchgit {
            url = "https://github.com/cmu-sei/Polar";
            rev = "f37b0b4";
            hash = "sha256-q7rl3G0Y0bQckDzltpYuO+KGt4CBSbuzenv2nuEs0S4=";
          };
          cargoHash = "sha256-0AF8oFLpvrf+CCSfOUlfDToJpTGbpCtKC3/TWtfpTmY=";
          sourceRoot = "${src.name}/src"; # Indicate to buildRustPackage where the base Cargo.toml is

          # These packages are dependencies necessary to build Polar
          nativeBuildInputs = with pkgs; [
            openssl
            openssl.dev
            pkg-config
          ];
          # These packages are dependencies necessary to run Polar
          buildInputs = with pkgs; [
            openssl
          ];
          doCheck = false; # turn off package checks (no network connection breaks some of them)
        };
      
        myEnv = pkgs.buildEnv {
          name = "my-env";
          paths = with pkgs; [
            polar

            # Include bash command line & some critical utilities
            bash
            coreutils-full
          ];
          pathsToLink = [
            "/bin"
            "/etc/ssl/certs"
          ];
        };
        in
        { # The docker image assembled here should be started using the companion docker compose file.
          packages.default = pkgs.dockerTools.buildImage {
            name = "polar-run";
            tag = "latest";
            copyToRoot = [ myEnv ];
            config = {
              WorkingDir = "/workspaces";
              Env = [  ];
              Volumes = { };
              Entrypoint = [ "/bin/bash" "-c" "echo 'The license for this container can be found in /workspaces/license.txt'; exec \"$@\"" "--" ];
              Cmd = [ "/bin/bash" ];
            };
            extraCommands = ''
              # Create /tmp dir
              mkdir -p tmp
              
              mkdir workspaces
              cp ${polar.src}/license.txt workspaces
              cp -r ${polar}/bin/* workspaces
            '';
          };
        }
    );
}