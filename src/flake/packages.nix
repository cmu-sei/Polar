# This file defines packages intended for use in our various environments.
#
{
  system,
  pkgs,
  rust-overlay,
  staticanalysis,
  dotacat
}:
let
    rustSysroot = pkgs.buildEnv {
        name = "rust-sysroot";
        paths = [
          pkgs.glibc
          pkgs.glibc.dev
          pkgs.gcc.cc.lib
        ];
    };

    clangLldWrapper = pkgs.writeShellScriptBin "clang-lld-wrapper" ''
      exec ${pkgs.llvmPackages_19.clang}/bin/clang \
        -fuse-ld=lld \
        --sysroot=${rustSysroot} \
        "$@"
    '';

in
  {
    devPkgs = with pkgs; [
      # -- Basic Required Files --
      bash # Basic bash to run bare essential code
      glibcLocalesUtf8
      uutils-coreutils-noprefix # Essential GNU utilities (ls, cat, etc.)

      gnugrep # GNU version of grep for searching text
      gnused # GNU version of sed for text processing
      gnutar # GNU version of tar for archiving
      gzip # Compression utility

      # -- FISH! --
      figlet
      fish
      fishPlugins.bass
      fishPlugins.bobthefish
      fishPlugins.foreign-env
      fishPlugins.grc
      cowsay
      starship
      atuin

      # -- OpenSSL --
      cacert
      dropbear
      openssh
      openssl
      openssl.dev

      # -- Development tools --
      bat
      curl
      delta
      direnv
      eza
      fd
      findutils
      fzf
      gawk
      getent
      git
      gnugrep
      iproute2
      jq
      lsof
      man
      man-db
      man-pages
      man-pages-posix
      ncurses
      nix
      nvim-pkg
      procps
      ps
      ripgrep
      rsync
      rustlings
      strace
      tree
      tree-sitter
      which

      dhall
      dhall-yaml
      dhall-json

      # -- Compilers, Etc. --
      cmake
      gnumake
      # clang or clang-tools are not strictly needed if stdenv is clang-based
      # but you can add them if you want the standalone `clang` CLI, e.g.:
      pkgs.llvmPackages_19.clang
      #pkgs.llvmPackages_19.clang-unwrapped
      pkgs.llvmPackages_19.lld
      glibc
      clangLldWrapper

      grc

      # -- Rust --
      (lib.meta.hiPrio (rust-bin.nightly.latest.default.override {
        extensions = [ "rust-src" "rust-analyzer" ];
        targets = [ "wasm32-unknown-unknown" ];
      }))

      wasm-pack
      wasmtime
      wasmer
      wasmer-pack
      wasm-bindgen-cli_0_2_100
      cargo-leptos
      cargo-binutils
      cargo-wasi
      pkg-config
      trunk
      util-linux

      # The last editor you'll ever use
      zed

      # Put any extra packages or libraries you need here. For example,
      # if working on a Rust project that requires a linear algebra
      # package:
      # openblas

      # -- Static Analysis Tools --
      staticanalysis.packages.${system}.default

      dotacat.packages.${system}.default
      vulnix
      skopeo
      grype
      syft
      sops
      envsubst
    ];

    # Set up the packages we want to include in our CI and testing environments,
    ciPkgs = with pkgs; [
      # -- Basic Required Files --
      bash # Basic bash to run bare essential code
      glibcLocalesUtf8
      uutils-coreutils-noprefix # Essential GNU utilities (ls, cat, etc.)
      curl
      gnugrep # GNU version of grep for searching text
      gnused # GNU version of sed for text processing
      gnutar # GNU version of tar for archiving
      gzip # Compression utility
      findutils
      cacert
      openssl
      nix
      git
      dhall
      dhall-yaml
      dhall-json
      jq
      yq
      vulnix
      skopeo
      grype
      syft
      sops
      envsubst
    ];
  }
