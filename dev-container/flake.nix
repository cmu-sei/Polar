{
  description = "creates a dev container for polar";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils"; # Utility functions for Nix flakes
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable"; # Main Nix package repository
    rust-overlay.url = "github:oxalica/rust-overlay?rev=260ff391290a2b23958d04db0d3e7015c8417401";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    rust-overlay.inputs.flake-utils.follows = "flake-utils";
    myNeovimOverlay.url = "github:daveman1010221/nix-neovim";
    myNeovimOverlay.inputs.nixpkgs.follows = "nixpkgs";
    myNeovimOverlay.inputs.flake-utils.follows = "flake-utils";
    nix-vscode-extensions.url = "github:nix-community/nix-vscode-extensions";
    nix-vscode-extensions.inputs.nixpkgs.follows = "nixpkgs";
    nix-vscode-extensions.inputs.flake-utils.follows = "flake-utils";
    staticanalysis.url = "github:rmdettmar/polar-static-analysis";
    staticanalysis.inputs.nixpkgs.follows = "nixpkgs";
    staticanalysis.inputs.flake-utils.follows = "flake-utils";
    staticanalysis.inputs.rust-overlay.follows = "rust-overlay";
  };

  outputs = { self, flake-utils, nixpkgs, rust-overlay, myNeovimOverlay, nix-vscode-extensions, staticanalysis, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default myNeovimOverlay.overlays.default ];
        };
        # This is needed since VSCode Devcontainers need the following files in order to function.
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

        extensions = nix-vscode-extensions.extensions.${system};

        code-extended = pkgs.vscode-with-extensions.override {
          vscode = pkgs.code-server;
          vscodeExtensions = [
            extensions.open-vsx-release.rust-lang.rust-analyzer
            extensions.vscode-marketplace.vadimcn.vscode-lldb # does not work yet - known bug
            extensions.vscode-marketplace.fill-labs.dependi
            extensions.vscode-marketplace.tamasfe.even-better-toml
            extensions.vscode-marketplace.jnoortheen.nix-ide
            extensions.vscode-marketplace.jinxdash.prettier-rust
            extensions.vscode-marketplace.dustypomerleau.rust-syntax
            extensions.vscode-marketplace.ms-vscode.test-adapter-converter
            extensions.vscode-marketplace.hbenl.vscode-test-explorer # dependency for rust test adapter
            extensions.vscode-marketplace.swellaby.vscode-rust-test-adapter
            extensions.vscode-marketplace.vscodevim.vim
            extensions.vscode-marketplace.redhat.vscode-yaml
            extensions.vscode-marketplace.ms-azuretools.vscode-docker
          ];
        };

        myEnv = pkgs.buildEnv {
          name = "my-env";
          paths = with pkgs; [
            # -- Basic Required Files --
            bash # Basic bash to run bare essential code
            uutils-coreutils-noprefix # Essential GNU utilities (ls, cat, etc.)

            # -- Needed for VSCode dev container --
            gnutar # GNU version of tar for archiving 
            gzip # Compression utility
            gnugrep # GNU version of grep for searching text
            gnused # GNU version of sed for text processing
            pkgs.stdenv.cc.cc.lib # Standard C library needed for linking C++ programs

            # -- FISH! --
            fish
            fishPlugins.bass
            fishPlugins.bobthefish
            fishPlugins.foreign-env
            fishPlugins.grc
            figlet
            lolcat

            # -- OpenSSL --
            cacert
            openssl
            openssl.dev

            # -- Development tools --
            code-extended
            rust-analyzer

            which
            nvim-pkg
            curl
            lsof
            strace
            ripgrep
            tree
            tree-sitter
            nix
            git
            fzf
            fd
            eza
            findutils
            gnugrep
            getent
            gawk
            jq
            ps
            ncurses

            # -- Compilers, Etc. --
            gcc
            grc
            cmake
            gnumake
            libclang
            python3
            glibc

            # -- Rust --
            #(lib.meta.hiPrio rust-bin.nightly.latest.default)

            # We need to support various WASM targets, possibly ARM64 targets.
            # This allows us to select those. Also, by default, we should
            # include the sources for Rust, so that the debugger works properly
            # and can jump to definition.
            (rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
              extensions = [ "rust-src" ];
              targets = [ "wasm32-unknown-unknown" "wasm32-wasip1" ];
            }))
            cargo-wasi
            cargo-leptos
            pkg-config
            trunk

            # -- Static Analysis Tools --
            staticanalysis.packages.${system}.default
          ];
          pathsToLink = [
            "/bin"
            "/lib"
            "/inc"
            "/etc/ssl/certs"
          ];
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

        codeSettings = pkgs.writeTextFile {
          name = "container-files/settings.json";
          destination = "/root/.local/share/code-server/User/settings.json";
          text = builtins.readFile ./container-files/settings.json;
        };

        license = pkgs.writeTextFile {
          name = "container-files/license.txt";
          destination = "/root/license.txt";
          text = builtins.readFile ./container-files/license.txt;
        };

        # User creation script
        createUserScript = pkgs.writeTextFile {
          name = "container-files/create-user.sh";
          destination = "/create-user.sh";
          text = builtins.readFile ./container-files/create-user.sh;
          executable = true;
        };

      in
      {
        packages.default = pkgs.dockerTools.buildImage {
          name = "polar-dev";
          tag = "latest";
          copyToRoot = [ myEnv baseInfo fishConfig codeSettings license createUserScript fishPluginsFile ];
          config = {
            WorkingDir = "/workspace";
            Env = [
              # Add certificates to allow for cargo to download files from the internet
              "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
              "SSL_CERT_DIR=/etc/ssl/certs"
              "CARGO_HTTP_CAINFO=/etc/ssl/certs/ca-bundle.crt"
              "CC=gcc" # Set GCC as the default C compiler
              "CXX=g++" # Set G++ as the default C++ compiler
              # Library path for dynamic linking
              "LD_LIBRARY_PATH=${pkgs.stdenv.cc.cc.lib}/lib"
              # Add openssl to pkg config to ensure that it loads for cargo build
              "PKG_CONFIG_PATH=${pkgs.openssl.dev}/lib/pkgconfig"
              # Setting PATH to include essential binaries
              "PATH=/bin:/usr/bin:${myEnv}/bin:/root/.cargo/bin"
              "USER=root" # Setting user to root
              "COREUTILS=${pkgs.uutils-coreutils-noprefix}"
              "CMAKE=/bin/cmake"
              "CMAKE_MAKE_PROGRAM=/bin/make"
              "LIBCLANG_PATH=${pkgs.libclang.lib}/lib/"
              "SHELL=/bin/fish"

              # Fish plugins
              "FISH_GRC=${pkgs.fishPlugins.grc}"
              "FISH_BASS=${pkgs.fishPlugins.bass}"
              "BOB_THE_FISH=${pkgs.fishPlugins.bobthefish}"
            ];
            Volumes = { };
            Cmd = [ "/bin/fish" ]; # Runs fish
          };
          extraCommands = ''
            # Link the env binary (needed for the check requirements script)
            mkdir -p usr/bin/
            ln -n bin/env usr/bin/env 

            # Link the dynamic linker/loader (needed for Node within vscode server)
            mkdir -p lib64 
            ln -s ${pkgs.glibc}/lib/ld-linux-x86-64.so.2 lib64/ld-linux-x86-64.so.2 

            # Create /tmp dir
            mkdir -p tmp
          '';
        };
      }
    );
}
