# Define packages intended for use in the a typical developer environment
#
{
  pkgs
}:

{
  devPaths = with pkgs; [
    # -- Basic Required Files --
    bash # Basic bash to run bare essential code
    glibcLocalesUtf8
    uutils-coreutils-noprefix # Essential GNU utilities (ls, cat, etc.)

    # -- Needed for VSCode dev container --
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
    lolcat

    # -- OpenSSL --
    cacert
    openssl
    openssl.dev

    # -- Development tools --
    bat
    code-extended
    curl
    delta
    eza
    fd
    findutils
    fzf
    gawk
    getent
    git
    gnugrep
    jq
    lsof
    man-db
    man-pages
    man-pages-posix
    ncurses
    nix
    nvim-pkg
    ps
    ripgrep
    rust-analyzer
    rustlings
    strace
    tree
    tree-sitter
    which

    dhall
    dhall-to-yaml
    dhall-to-json

    # -- Compilers, Etc. --
    cmake
    gnumake
    # clang or clang-tools are not strictly needed if stdenv is clang-based
    # but you can add them if you want the standalone `clang` CLI, e.g.:
    pkgs.llvmPackages_19.clang
    lld
    glibc
    grc

    # -- Rust --
    (lib.meta.hiPrio rust-bin.nightly.latest.default)

    cargo-leptos
    cargo-wasi
    pkg-config
    trunk
    util-linux
    
    # Put any extra packages or libraries you need here. For example,
    # if working on a Rust project that requires a linear algebra
    # package:
    # openblas

    # -- Static Analysis Tools --
    staticanalysis.packages.${system}.default
  ];

  # Set up the packages we want to include in our CI and testing environments,
  # on top of the official nix image which can facilitate most operations
  ciPkgs = with pkgs; [
    bash # Basic bash to run bare essential code
    glibcLocalesUtf8
    uutils-coreutils-noprefix # Essential GNU utilities (ls, cat, etc.)
    
    # -- OpenSSL --
    cacert
    openssl
    openssl.dev

    nix
    dhall-yaml
    dhall-json
    jq
    yq
    vulnix
    kubernetes-helm
    skopeo
    
    l
  ];
}