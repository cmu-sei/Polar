# src/flake/ci-container.nix
#
# Builds the Polar CI container image.
# Single-user Nix install — no daemon, no nixbld group, no privilege separation.
#
# Import from the root flake:
#   ciContainer = import ./src/flake/ci-container.nix {
#     inherit pkgs lib system;
#     cassiniClient = polarPkgs.cassini.client;
#   };
#   ciImage = ciContainer.image;

{ pkgs
, lib
, system
, cassiniClient
, certIssuerClient
}:

let
  rustToolchain = pkgs.rust-bin.nightly.latest.default.override {
    extensions = [ "rust-src" "rust-std" "clippy" "rustfmt" "llvm-tools-preview" ];
  };

  llvm = pkgs.llvmPackages_19;
  nu   = pkgs.nushell;

  # ── /etc identity files ───────────────────────────────────────────────────
  # 32 nixbld users — matches the official NixOS container exactly.
  # Generated statically at build time via builtins.genList; no runtime init.
  buildUserCount = 32;

  nixbldUsers = builtins.genList (n: {
    name = "nixbld${toString (n + 1)}";
    uid  = 30000 + n + 1;
    gid  = 30000;
  }) buildUserCount;

  passwdEntries = builtins.concatStringsSep "\n" (
    [ "root:x:0:0::/root:${pkgs.bash}/bin/bash"
      "nobody:x:65534:65534:Nobody:/var/empty:/sbin/nologin"
    ]
    ++ map (u: "${u.name}:x:${toString u.uid}:${toString u.gid}:Nix build user:/var/empty:/sbin/nologin") nixbldUsers
  ) + "\n";

  nixbldGroupEntry = "nixbld:x:30000:"
    + builtins.concatStringsSep "," (map (u: u.name) nixbldUsers);

  groupEntries = builtins.concatStringsSep "\n" [
    "root:x:0:root"
    nixbldGroupEntry
    "nobody:x:65534:nobody"
  ] + "\n";

  shadowEntries = builtins.concatStringsSep "\n" (
    [ "root:!x:::::::" ]
    ++ map (u: "${u.name}:!:::::::") nixbldUsers
  ) + "\n";

  gshadowEntries = builtins.concatStringsSep "\n" [
    "root:x::"
    ("nixbld:!::" + builtins.concatStringsSep "," (map (u: u.name) nixbldUsers) + ":")
  ] + "\n";

  shellsFile = "${pkgs.bash}/bin/bash\n${nu}/bin/nu\n";

  osRelease = ''
    NAME="Polar CI"
    ID=polar-ci
    VERSION="unstable"
    PRETTY_NAME="Polar CI (nixpkgs unstable)"
  '';

  # writeTextDir produces symlinks into the Nix store. containerd rejects
  # symlinks in /etc that escape the container root with exit code 126.
  # runCommand writes real files — no symlinks, no containerd complaints.
  baseInfo = pkgs.runCommand "base-info" {} ''
    mkdir -p $out/etc $out/var/empty
    printf '%s' '${passwdEntries}'   > $out/etc/passwd
    printf '%s' '${groupEntries}'    > $out/etc/group
    printf '%s' '${shadowEntries}'   > $out/etc/shadow
    printf '%s' '${gshadowEntries}'  > $out/etc/gshadow
    printf '%s' '${shellsFile}'      > $out/etc/shells
    printf '%s' '${osRelease}'       > $out/etc/os-release
    chmod 640 $out/etc/shadow $out/etc/gshadow
  '';

  # ── config files as real files ───────────────────────────────────────────
  # All written via runCommand so containerd gets real files, not symlinks.
  nuConfig = pkgs.writeTextFile {
    name        = "nu-config";
    destination = "/etc/nushell/config.nu";
    text        = ''
      $env.config = {
        show_banner: false
        edit_mode:   vi
        history: {
          max_size:      0
          sync_on_enter: false
          file_format:   "plaintext"
        }
        cursor_shape: { vi_insert: line, vi_normal: block, emacs: line }
        completions: {
          case_sensitive: false
          quick:          true
          partial:        true
          algorithm:      "fuzzy"
          external:       { enable: true, max_results: 50 }
        }
        table: {
          mode:       rounded
          index_mode: always
          show_empty: true
          padding:    { left: 1, right: 1 }
          trim: {
            methodology:             wrapping
            wrapping_try_keep_words: true
            truncating_suffix:       "..."
          }
        }
        error_style: "fancy"
      }
    '';
  };

  nuEnv = pkgs.writeTextFile {
    name        = "nu-env";
    destination = "/etc/nushell/env.nu";
    text        = ''
      $env.SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      $env.SSL_CERT_DIR  = "${pkgs.cacert}/etc/ssl/certs"
      $env.SHELL         = "${nu}/bin/nu"
    '';
  };

  nixConf = pkgs.writeTextFile {
    name        = "nix-conf";
    destination = "/etc/nix/nix.conf";
    text        = ''
      build-users-group = nixbld
      sandbox = false
      experimental-features = nix-command flakes
      extra-trusted-users = root
      keep-outputs = true
      keep-derivations = true
      substituters = https://cache.nixos.org
      trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=
    '';
  };

  nixRegistry = pkgs.writeTextFile {
    name        = "nix-registry";
    destination = "/etc/nix/registry.json";
    text        = builtins.toJSON {
      version = 2;
      flakes  = [{
        from = { type = "indirect"; id = "nixpkgs"; };
        to   = { type = "path"; path = "${pkgs.path}"; };
      }];
    };
  };

  containerPolicy = pkgs.writeTextFile {
    name        = "container-policy";
    destination = "/etc/containers/policy.json";
    text        = builtins.toJSON {
      default = [{ type = "insecureAcceptAnything"; }];
      transports.docker-daemon."" = [{ type = "insecureAcceptAnything"; }];
    };
  };

  ciContents = with pkgs; [
    rustToolchain
    nu
    nuConfig
    nuEnv
    nix
    nixConf
    nixRegistry
    containerPolicy
    git
    curl
    cacert
    coreutils
    findutils
    gnugrep
    gnused
    gnutar
    gzip
    gawk
    bash
    which
    procps
    openssl
    openssl.dev
    pkg-config
    llvm.clang
    llvm.libclang
    llvm.libclang.lib
    zlib
    zlib.dev
    cargo-deny
    cargo-udeps
    cargo-semver-checks
    cargo-geiger
    cargo-hack
    cargo-mutants
    cargo-cyclonedx
    skopeo
    cosign
    cassiniClient
    certIssuerClient
    vim
    buildah
  ];

  image = pkgs.dockerTools.buildLayeredImage {
    name      = "polar-ci";
    tag       = "latest";
    contents  = ciContents;
    maxLayers = 20;

    extraCommands = ''
      # /usr/bin/env — expected by many build scripts
      mkdir -p usr/bin
      ln -s ${pkgs.coreutils}/bin/env usr/bin/env


      mkdir -p etc
      cp ${baseInfo}/etc/passwd   etc/passwd
      cp ${baseInfo}/etc/group    etc/group
      cp ${baseInfo}/etc/shadow   etc/shadow
      cp ${baseInfo}/etc/gshadow  etc/gshadow
      cp ${baseInfo}/etc/shells   etc/shells
      cp ${baseInfo}/etc/os-release etc/os-release
      chmod 644 etc/passwd etc/group etc/shells etc/os-release
      chmod 640 etc/shadow etc/gshadow

      # Dynamic linker stub — needed by any dynamically linked binary
      ${if system == "aarch64-linux" then ''
        mkdir -p lib
        ln -s ${pkgs.glibc}/lib/ld-linux-aarch64.so.1 lib/ld-linux-aarch64.so.1
      '' else ''
        mkdir -p lib64
        ln -s ${pkgs.glibc}/lib/ld-linux-x86-64.so.2 lib64/ld-linux-x86-64.so.2
      ''}

      # FHS dirs
      mkdir -p tmp var/tmp var/empty var/cache/cargo-target
      chmod 1777 tmp var/tmp

      # Nix store DB dir — nix-store --load-db writes here at first use
      mkdir -p nix/var/nix/db
      mkdir -p nix/var/nix/gcroots
      mkdir -p nix/var/nix/profiles

      # Root home
      mkdir -p root/.cargo/bin
      chmod 700 root
    '';

    config = {
      User       = "0:0";
      WorkingDir = "/workspace";
      Cmd        = [ "${nu}/bin/nu"];
      Env = [
        "OPENSSL_DIR=${pkgs.openssl.dev}"
        "OPENSSL_LIB_DIR=${pkgs.openssl.out}/lib"
        "OPENSSL_INCLUDE_DIR=${pkgs.openssl.dev}/include"
        "PKG_CONFIG_PATH=${pkgs.openssl.dev}/lib/pkgconfig:${pkgs.zlib.dev}/lib/pkgconfig"
        "LIBCLANG_PATH=${llvm.libclang.lib}/lib"
        "CC=${llvm.clang}/bin/clang"
        "CXX=${llvm.clang}/bin/clang++"
        "RUSTFLAGS=-Clinker=${llvm.clang}/bin/clang"
        "CARGO_HOME=/root/.cargo"
        "CARGO_TARGET_DIR=/var/cache/cargo-target"
        "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "SSL_CERT_DIR=${pkgs.cacert}/etc/ssl/certs"
        "CARGO_HTTP_CAINFO=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "GIT_SSL_CAINFO=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        "NIX_SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
        # HOME=/root is the homeless-shelter fix — Nix checks this path
        # exists before running builds; /root always exists as root.
        "HOME=/root"
        "USER=root"
        "SHELL=${nu}/bin/nu"
        # Empty string tells Nix single-user: no daemon socket to connect to
        "NIX_REMOTE="
        "PATH=${pkgs.nix}/bin:/root/.cargo/bin:${nu}/bin:${pkgs.git}/bin:${pkgs.coreutils}/bin:${pkgs.bash}/bin:/usr/bin:/bin"
      ];
      Volumes = {
        "/workspace"              = {};
        "/var/cache/cargo-target" = {};
      };
    };
  };

in { inherit image; }
