{
  description = "static analysis tools for Polar";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils"; # Utility functions for Nix flakes
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable"; # Main Nix package repository
    rust-overlay.url = "github:oxalica/rust-overlay";
    l3x.url = "github:rmdettmar/l3x?dir=l3x"; # l3x repo with nix support
  };

  outputs = { self, flake-utils, nixpkgs, rust-overlay, l3x, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        audit = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cargo-audit";
          version = "0.20.0";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-hzy+AVWGWzWYupllrLSryoi4rXPM0+G6WBlRbf03xA8=";
          };
          cargoHash = "sha256-OOkJGdqEHNVbgZZIjQupGaSs4tB52b7kPGLKELUocn4=";
        };

        auditable = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cargo-auditable";
          version = "0.6.4";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-5DJRLjTMOHAhHHunNny7YXKEQoezRb+A8ORPH2WXQ3o=";
          };
          cargoHash = "sha256-TTR1mhsA9612GR/OVGsp6QmYceasSl4nf827TKCfxPM=";
          doCheck = false; # turn off package checks (which don't work in the nix environment)
        };

        bloat = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cargo-bloat";
          version = "0.12.1";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-9uWQNaRt5U09YIiAFBLUgcHWm2vg2gazSjtwR1+It3M=";
          };
          cargoHash = "sha256-BBFLyMx1OPT2XAM6pofs2kV/3n3FrNu0Jkyr/Y3smnI=";

        };

        semvers = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cargo-semver-checks";
          version = "0.32.0";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-wz2HT40oGGhCn2f1Co/aLhcanjsv5QAXUFXRXletITw=";
          };
          cargoHash = "sha256-S03fgnefhU6c5e9YtFMBart+nfBQj7f4O+lSPe8fgqg=";
          buildInputs = with pkgs; [
            cmake
            gnumake
          ];
          preHook = ''
            export CMAKE="${pkgs.cmake}/bin/cmake"
            export CMAKE_MAKE_PROGRAM="${pkgs.gnumake}/bin/make"
          '';
          doCheck = false; # turn off package checks (which don't work in the nix environment)
        };

        # this works when you cargo-install it
        spellcheck = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cargo-spellcheck";
          version = "0.14.0";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-Ywc0/y8SE/hk+keZbnblMHSfm1HI1dCJ9i97qEG50v0=";
          };
          cargoHash = "sha256-7zZ3Erb8LwGor9X3cd4TRpFLfO4DXMzkdexPfqyrkzk=";
          buildInputs = with pkgs; [
            libclang
          ];
          preHook = ''
            export LIBCLANG_PATH="${pkgs.libclang.lib}/lib/"
          '';
          doCheck = false; # turn off package checks (which don't work in the nix environment)
        };

        deny = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cargo-deny";
          version = "0.14.24";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-CN+HESgzxcAf4msb914GvyeRcqX8aW9sbSxdT+kZkb8=";
          };
          cargoHash = "sha256-LCdP9i+LogdPVVCI4UIhqGRy6H3GTMpEwX2QOlXbo8Q=";
          doCheck = false; # turn off package checks (which don't work in the nix environment)
        };

        unusedfeatures = pkgs.rustPlatform.buildRustPackage rec {
          pname = "cargo-unused-features";
          version = "0.2.0";
          src = pkgs.fetchCrate {
            inherit pname version;
            hash = "sha256-gdwIbbQDw/DgBV9zY2Rk/oWjPv1SS/+oFnocsMo2Axo=";
          };
          cargoHash = "sha256-K9I7Eg43BS2SKq5zZ3eZrMkmuHAx09OX240sH0eGs+k=";

          nativeBuildInputs = with pkgs; [
            openssl
            openssl.dev
            pkg-config
          ];
          buildInputs = with pkgs; [
            openssl
            openssl.dev
            pkg-config
          ];
        };

        in {
          packages.default = pkgs.symlinkJoin {
            name = "polar-static-analysis-tools";
            paths = with pkgs; [
              audit
              auditable
              bloat
              semvers
              spellcheck
              noseyparker
              cargo-udeps
              deny
              unusedfeatures
              l3x.packages.${system}.default
            ];
          };
        }

    );
}