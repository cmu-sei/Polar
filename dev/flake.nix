{
  description = "creates a dev container for polar";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils"; # Utility functions for Nix flakes
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable"; # Main Nix package repository

    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";

    myNeovimOverlay.url = "github:daveman1010221/nix-neovim";
    myNeovimOverlay.inputs.nixpkgs.follows = "nixpkgs";
    myNeovimOverlay.inputs.flake-utils.follows = "flake-utils";

    staticanalysis.url = "github:daveman1010221/polar-static-analysis";
    staticanalysis.inputs.nixpkgs.follows = "nixpkgs";
    staticanalysis.inputs.flake-utils.follows = "flake-utils";
    staticanalysis.inputs.rust-overlay.follows = "rust-overlay";

    dotacat.url = "github:daveman1010221/dotacat-fast";
    dotacat.inputs.nixpkgs.follows = "nixpkgs";

    #openssl-fips.url = "github:daveman1010221/openssl-fips";
  };

  outputs = { flake-utils, nixpkgs, rust-overlay, myNeovimOverlay, staticanalysis, dotacat, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let

        pkgs = import nixpkgs {
          inherit system;
          config = {
            cc = "clang";
          };
          documentation = {
            dev.enable = true;
            man = {
              man-db.enable = true;
              generateCaches = true;
            };
          };
          overlays = [ 
            rust-overlay.overlays.default
            myNeovimOverlay.overlays.default
          ];
        };

        #import package sets to be added to our environments
        packageSets = import ./packages.nix {inherit system pkgs rust-overlay staticanalysis dotacat; };

        # This is needed since Devcontainers need the following files in order to function.
        baseInfo = with pkgs; [
          # Set up shadow file with user information
          (writeTextDir "etc/shadow" ''
            root:!x:::::::
          '')
          # Set up passwd file with user information
          (writeTextDir "etc/passwd" ''
            root:x:0:0::/root:${runtimeShell}
          '')
          # Set up group file with user information
          (writeTextDir "etc/group" ''
            root:x:0:
          '')
          # Set up gshadow file with user information
          (writeTextDir "etc/gshadow" ''
            root:x::
          '')
          # Set up os-release file with NixOS information, since it is nix the check requirements
          # step for the dev container creation will skip.
          (writeTextDir "etc/os-release" ''
            NAME="NixOS"
            ID=nixos
            VERSION="unstable"
            VERSION_CODENAME=unstable
            PRETTY_NAME="NixOS (unstable)"
            HOME_URL="https://nixos.org/"
            SUPPORT_URL="https://nixos.org/nixos/manual/"
            BUG_REPORT_URL="https://github.com/NixOS/nixpkgs/issues"
          '')
        ];

        devEnv = pkgs.buildEnv {
          name = "dev-env";
          paths = packageSets.devPkgs;
          pathsToLink = [
            "/bin"
            "/lib"
            "/inc"
            "/etc/ssl/certs"
          ];
        };

        ciEnv = pkgs.buildEnv {
          name = "ci-env";
          paths = packageSets.ciPkgs;
          pathsToLink = [
            "/bin"
            "/lib"
            "/inc"
            "/etc/ssl/certs"
          ];
        };

        nixConfig = pkgs.writeTextFile {
          name = "nix.conf";
          destination = "/etc/nix/nix.conf";
          text = builtins.readFile ./container-files/nix.conf;
        };

        containerPolicyConfig = pkgs.writeTextFile {
          name = "nix.conf";
          destination = "/etc/containers/policy.json";
          text = builtins.readFile ./container-files/policy.json;
        }; 

        fishConfig = pkgs.writeTextFile {
          name = "container-files/config.fish";
          destination = "/root/.config/fish/config.fish";
          text = builtins.readFile ./container-files/config.fish;
        };

        fishPluginsFile = pkgs.writeTextFile {
          name = "container-files/plugins.fish";
          destination = "/.plugins.fish";
          text = builtins.readFile ./container-files/plugins.fish;
        };

        license = pkgs.writeTextFile {
          name = "container-files/license.txt";
          destination = "/root/license.txt";
          text = builtins.readFile ./container-files/license.txt;
        };

        gitconfig = pkgs.writeTextFile {
          name = "container-files/.gitconfig";
          destination = "/root/.gitconfig";
          text = builtins.readFile ./container-files/.gitconfig;
        };

        # User creation script
        createUserScript = pkgs.writeTextFile {
          name = "container-files/create-user.sh";
          destination = "/create-user.sh";
          text = builtins.readFile ./container-files/create-user.sh;
          executable = true;
        };

        # get helm charts
        charts = pkgs.callPackage ./make-chart.nix { inherit pkgs; };
        
        devContainer = pkgs.dockerTools.buildImage {
          name = "polar-dev";
          tag = "latest";
          copyToRoot = [
            devEnv
            baseInfo
            fishConfig
            license
            gitconfig
            createUserScript
            fishPluginsFile
            nixConfig
          ];
          config = {
            WorkingDir = "/workspace";
            Env = [
              "CARGO_HTTP_CAINFO=/etc/ssl/certs/ca-bundle.crt"

              # Fish plugins
              "BOB_THE_FISH=${pkgs.fishPlugins.bobthefish}"
              "FISH_BASS=${pkgs.fishPlugins.bass}"
              "FISH_GRC=${pkgs.fishPlugins.grc}"

              "CC=clang"
              "CXX=clang++"
              "LD=ld.lld"
              "CMAKE=/bin/cmake"
              "CMAKE_MAKE_PROGRAM=/bin/make"
              "COREUTILS=${pkgs.uutils-coreutils-noprefix}"

              "LANG=en_US.UTF-8"
              "TZ=UTC"
              "MANPAGER=sh -c 'col -bx | bat --language man --style plain'"
              "MANPATH=${pkgs.man-db}/share/man:$MANPATH"
              "LOCALE_ARCHIVE=${pkgs.glibcLocalesUtf8}/lib/locale/locale-archive"

              # stdenv.cc is clang-based now, so this is fine:
              "LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [ devEnv pkgs.stdenv.cc.cc.lib ]}"

              #"LIBCLANG_PATH=${pkgs.libclang.lib}/lib/"

              "RUSTFLAGS=-Clinker=clang"

              "PATH=/bin:/usr/bin:${devEnv}/bin:/root/.cargo/bin"

              # Add openssl to pkg config to ensure that it loads for cargo build
              "PKG_CONFIG_PATH=${pkgs.openssl.dev}/lib/pkgconfig"

              "SHELL=/bin/fish"
              "SSL_CERT_DIR=/etc/ssl/certs"

              # Add certificates to allow for cargo to download files from the
              # internet. May have to adjust this for FIPS.
              "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"

              "USER=root"
            ];
            Volumes = {};
            Cmd = [ "/bin/fish" ]; # Runs fish
          };
          extraCommands = ''
            # Link the env binary (needed for the check requirements script)
            mkdir -p usr/bin/
            ln -n bin/env usr/bin/env

            # Link the dynamic linker/loader (needed for Node)
            mkdir -p lib64
            ln -s ${pkgs.stdenv.cc.cc}/lib/ld-linux-x86-64.so.2 lib64/ld-linux-x86-64.so.2

            # Create /tmp dir
            mkdir -p tmp
          '';
        };

        # So this build is a little hacky, we'd usually be able to use nix2container to just put some creds in a
        # auth.json file and allow the daemon to see it, but we need to pull from a registry behind a proxy,
        # and nix2container doesn't let us do that w/o disabling tlsverification. 
        # We can switch to do that if this approach is too brittle.
        ciContainer = pkgs.dockerTools.buildImage {
          name = "polar-ci";
          fromImage = ./nix.tar;
          #  pkgs.dockerTools.pullImage {
          #   imageName = "nixos/nix";
          #   imageDigest = "sha256:088c97f6f08320de53080138d595dda29226f8bc76b2ca8f617e4a739aefa8d7";
          #   hash = "sha256-5LpLRfMAVpMh03eAxocPAhLgi/klQlPiMAGSZwhUZr8=";
          #   finalImageName = "nixos/nix";
          #   finalImageTag = "2.24.13";
          # };
          tag = "0.1.0";
          copyToRoot = [
            license
            ciEnv
            nixConfig
            containerPolicyConfig
          ];
          config = {
            WorkingDir = "/workspace";
            Env = [
              # Add our tools to the path
              # CAUTION: This path value is taken directly from the base nix image and should not be overwritten, just appended to.
              # Whenever we bump to later versions of the nix image, this should be updated this as needed.
              "PATH=/root/.nix-profile/bin:/nix/var/nix/profiles/default/bin:/nix/var/nix/profiles/default/sbin:${ciEnv}/bin"
            ];
          };
          # extraCommands = ''
          #   # Link the env binary (needed for the check requirements script)
          #   mkdir -p usr/bin/

          #   ln -n bin/env usr/bin/env
            
          #   # Create /tmp dir
          #   mkdir -p tmp
          # '';
        };

      in
      {
        inherit devContainer ciContainer charts;

        devShells.default = pkgs.mkShell {
          name = "polar-devshell";
          packages = packageSets.devPkgs ++ [ pkgs.pkg-config pkgs.openssl ];
          shellHook = ''
            export OPENSSL_DIR="${pkgs.openssl.dev}"
            export OPENSSL_LIB_DIR="${pkgs.openssl.out}/lib"
            export OPENSSL_INCLUDE_DIR="${pkgs.openssl.dev}/include"
            export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig"
          '';
        };

        packages.default = devContainer;
        packages.ciContainer = ciContainer;
        packages.charts = charts;
      }
    );
}
