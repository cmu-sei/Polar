{
system,
pkgs,
rust-overlay,
staticanalysis,
dotacat,
myNeovimOverlay,
}:

let

    # packages for use in the images
    packageSets = import ./packages.nix { inherit system pkgs rust-overlay staticanalysis dotacat; };

    # ---------------------------------------------------------------------
    #  Files required by dev-containers.
    #  sets up users and permissions
    # ---------------------------------------------------------------------

    # Join user entries into each file format
    passwdEntries = builtins.concatStringsSep "\n" (
      ["root:x:0:0::/root:${pkgs.runtimeShell}"]
    ) + "\n";

    groupEntries = builtins.concatStringsSep "\n" (
      ["root:x:0:"]
    ) + "\n";

    gshadowEntries = builtins.concatStringsSep "\n" (
      ["root:x::"]
    ) + "\n";

    shadowEntries = builtins.concatStringsSep "\n" (
      ["root:!x:::::::"]
    ) + "\n";

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

    # ---------------------------------------------------------------------
    # Dev/CI envs
    # ---------------------------------------------------------------------
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

    # ---------------------------------------------------------------------
    # Misc config blobs copied verbatim
    # ---------------------------------------------------------------------
    nixConfig = pkgs.writeTextFile {
      name          = "nix.conf";
      destination   = "/etc/nix/nix.conf";
      text          = builtins.readFile ./container-files/nix.conf;
    };

    containerPolicyConfig = pkgs.writeTextFile {
      name        = "policy.json";
      destination = "/etc/containers/policy.json";
      text        = builtins.readFile ./container-files/policy.json;
    };

    # ---------------------------------------------------------------------
    # Fish vendor functions (system-wide) – this is what was missing.
    # ---------------------------------------------------------------------
    staticFuncPaths = pkgs.lib.mapAttrsToList
      (fileName: _: ./container-files/shell/fish/functions/static/${fileName})
      (builtins.readDir ./container-files/shell/fish/functions/static);

    templatedFuncPaths = pkgs.lib.mapAttrsToList
      (fileName: _:
        let name = pkgs.lib.removeSuffix ".nix" fileName;
        in pkgs.writeText "${name}.fish"
             (import ./container-files/shell/fish/functions/templated/${fileName} {
               inherit pkgs cowsayPath manpackage;
             }))
      (pkgs.lib.filterAttrs (n: _: pkgs.lib.hasSuffix ".nix" n)
        (builtins.readDir ./container-files/shell/fish/functions/templated));

    vendorFuncs = pkgs.runCommand "fish-vendor-funcs" { }
      (let all = staticFuncPaths ++ templatedFuncPaths;
           list = pkgs.lib.concatStringsSep " " all;
       in ''
         mkdir -p $out/etc/fish/vendor_functions.d
         for f in ${list}; do
           clean=$(basename "$f" | sed -E 's/^[0-9a-z]{32,}-//')   # drop Nix hash
           ln -s "$f" "$out/etc/fish/vendor_functions.d/$clean"
         done
       '');

    # ---------------------------------------------------------------------
    # Odds and ends
    # ---------------------------------------------------------------------
    cowsayPath  = pkgs.cowsay;
    manpackage  = pkgs.man;
    fisheyGrc   = pkgs.fishPlugins.grc;
    bass        = pkgs.fishPlugins.bass;
    bobthefish  = pkgs.fishPlugins.bobthefish;
    starshipBin = "${pkgs.starship}/bin/starship";
    atuinBin    = "${pkgs.atuin}/bin/atuin";
    editor      = myNeovimOverlay;
    fishShell   = pkgs.fish;

    # skeleton file for start.sh to copy
    fishConfig = pkgs.writeTextFile {
      name        = "fish-config";
      destination = "/etc/container-skel/config.fish";
      text        = builtins.readFile ./container-files/shell/fish/config.fish;
    };

    shellInitFile = pkgs.writeTextFile {
      name        = "shellInit.fish";
      destination = "/etc/fish/shellInit.fish";
      text        = import ./container-files/shell/fish/shellInit.nix { inherit pkgs; };
    };

    interactiveShellInitFile = pkgs.writeTextFile {
      name        = "interactiveShellInit.fish";
      destination = "/etc/fish/interactiveShellInit.fish";
      text        = import ./container-files/shell/fish/interactiveShellInit.nix {
        inherit fisheyGrc bass bobthefish starshipBin atuinBin editor fishShell;
      };
    };

    license = pkgs.writeTextFile {
      name        = "license.txt";
      destination = "/root/license.txt";
      text        = builtins.readFile ./container-files/license.txt;
    };

    gitconfig = pkgs.writeTextFile {
      name        = ".gitconfig";
      destination = "/root/.gitconfig";
      text        = builtins.readFile ./container-files/.gitconfig;
    };

    polarHelpScript = pkgs.writeShellScriptBin "polar-help" (
      builtins.readFile ./container-files/polar-help
    );

    startScript = pkgs.writeShellScriptBin "start.sh" (
      builtins.readFile ./container-files/start.sh
    );

    # ---------------------------------------------------------------------
    # Dev container image
    # ---------------------------------------------------------------------
    # devContainer = pkgs.dockerTools.buildImage {
    #   name = "polar-dev";
    #   tag  = "latest";
    #   copyToRoot = [
    #     baseInfo
    #     polarHelpScript
    #     startScript
    #     devEnv
    #     fishConfig
    #     gitconfig
    #     interactiveShellInitFile
    #     license
    #     nixConfig
    #     shellInitFile
    #     containerPolicyConfig
    #     vendorFuncs
    #   ];
    #   config = {
    #     WorkingDir = "/workspace";
    #     Env = [
    #       "CARGO_HTTP_CAINFO=/etc/ssl/certs/ca-bundle.crt"

    #       # Fish plugins
    #       "BOB_THE_FISH=${pkgs.fishPlugins.bobthefish}"
    #       "FISH_BASS=${pkgs.fishPlugins.bass}"
    #       "FISH_GRC=${pkgs.fishPlugins.grc}"

    #       "CC=clang"
    #       "CXX=clang++"
    #       "CMAKE=/bin/cmake"
    #       "CMAKE_MAKE_PROGRAM=/bin/make"
    #       "COREUTILS=${pkgs.uutils-coreutils-noprefix}"

    #       # tell clang-sys / bindgen where to find libclang
    #       "LIBCLANG_PATH=${pkgs.llvmPackages_19.libclang.lib}/lib"

    #       "LANG=en_US.UTF-8"
    #       "TZ=UTC"
    #       "MANPAGER=sh -c 'col -bx | bat --language man --style plain'"
    #       "MANPATH=${pkgs.man-db}/share/man:$MANPATH"
    #       "LOCALE_ARCHIVE=${pkgs.glibcLocalesUtf8}/lib/locale/locale-archive"

    #       # stdenv.cc is clang-based now, so this is fine:
    #       "LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [ devEnv pkgs.stdenv.cc.cc.lib ]}"

    #       #"LIBCLANG_PATH=${pkgs.libclang.lib}/lib/"

    #       "RUSTFLAGS=-Clinker=clang-lld-wrapper"

    #       "PATH=$PATH:/bin:/usr/bin:${devEnv}/bin:/root/.cargo/bin"

    #       # Add openssl to pkg config to ensure that it loads for cargo build
    #       "PKG_CONFIG_PATH=${pkgs.openssl.dev}/lib/pkgconfig"

    #       "SHELL=/bin/fish"
    #       "SSL_CERT_DIR=/etc/ssl/certs"

    #       # Add certificates to allow for cargo to download files from the
    #       # internet. May have to adjust this for FIPS.
    #       "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"

    #       "USER=root"
    #     ];
    #     Volumes = {};
    #     Cmd = [ "${startScript}/bin/start.sh" ];
    #   };
    #   extraCommands = ''
    #     # Link the env binary (needed for the check requirements script)
    #     mkdir -p usr/bin
    #     ln -s ${pkgs.coreutils}/bin/env usr/bin/env

    #     # Link the dynamic linker/loader (needed for Node)
    #     mkdir -p lib64
    #     ln -s ${pkgs.stdenv.cc.cc}/lib/ld-linux-x86-64.so.2 lib64/

    #     mkdir -p var/tmp
    #     mkdir -p tmp
    #   '';
    # };

    # ---------------------------------------------------------------------
    # Dev container image (layered)
    # ---------------------------------------------------------------------
    devContainer = pkgs.dockerTools.buildLayeredImage {
      name = "polar-dev";
      tag  = "latest";

      # Flatten baseInfo into the main list so we don't rely on implicit
      # flattening magic.
      contents = baseInfo ++ [
        polarHelpScript
        startScript
        devEnv
        fishConfig
        gitconfig
        interactiveShellInitFile
        license
        nixConfig
        shellInitFile
        containerPolicyConfig
        vendorFuncs
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
          "CMAKE=/bin/cmake"
          "CMAKE_MAKE_PROGRAM=/bin/make"
          "COREUTILS=${pkgs.uutils-coreutils-noprefix}"

          "LIBCLANG_PATH=${pkgs.llvmPackages_19.libclang.lib}/lib"

          "LANG=en_US.UTF-8"
          "TZ=UTC"
          "MANPAGER=sh -c 'col -bx | bat --language man --style plain'"
          "MANPATH=${pkgs.man-db}/share/man:$MANPATH"
          "LOCALE_ARCHIVE=${pkgs.glibcLocalesUtf8}/lib/locale/locale-archive"

          "LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [ devEnv pkgs.stdenv.cc.cc.lib ]}"

          "RUSTFLAGS=-Clinker=clang-lld-wrapper"

          "PATH=$PATH:/bin:/usr/bin:${devEnv}/bin:/root/.cargo/bin"

          "PKG_CONFIG_PATH=${pkgs.openssl.dev}/lib/pkgconfig"

          "SHELL=/bin/fish"
          "SSL_CERT_DIR=/etc/ssl/certs"
          "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"

          "USER=root"
        ];
        Volumes = {};
        Cmd = [ "${startScript}/bin/start.sh" ];
      };

      # Works the same way as with buildImage – it just becomes the top layer.
      extraCommands = ''
        # Link the env binary (needed for the check requirements script)
        mkdir -p usr/bin
        ln -s ${pkgs.coreutils}/bin/env usr/bin/env

        # Link the dynamic linker/loader (needed for Node)
        mkdir -p lib64
        ln -s ${pkgs.stdenv.cc.cc}/lib/ld-linux-x86-64.so.2 lib64/

        mkdir -p var/tmp
        mkdir -p tmp
      '';

      # Optional: be greedy with shareable layers if you don't care about extending this image with Dockerfiles.
      maxLayers = 128;
    };

in {
  inherit devContainer;

  devShells.default = pkgs.mkShell {
    name = "polar-devshell";
    packages = packageSets.devPkgs ++ [ pkgs.pkg-config pkgs.openssl ];

    shellHook = ''
      export OPENSSL_DIR="${pkgs.openssl.dev}"
      export OPENSSL_LIB_DIR="${pkgs.openssl.out}/lib"
      export OPENSSL_INCLUDE_DIR="${pkgs.openssl.dev}/include"
      export PKG_CONFIG_PATH="${pkgs.openssl.dev}/lib/pkgconfig"
      export CC=clang
      export CXX=clang++
      export CMAKE=/bin/cmake
      export CMAKE_MAKE_PROGRAM=/bin/make
      export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [ pkgs.glibc pkgs.llvmPackages_19.clang ]}"
      export LIBCLANG_PATH="${pkgs.llvmPackages_19.libclang.lib}/lib"

      # -------------------------------------------------------------------
      # Polar TLS auto-setup (direnv/nix develop)
      # -------------------------------------------------------------------
      set -euo pipefail

      PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"
      CERTS_LINK="$PROJECT_ROOT/result-tlsCerts"

      need_certs() {
        [ ! -e "$CERTS_LINK/ca_certificates/ca_certificate.pem" ] || \
        [ ! -e "$CERTS_LINK/server/server_cassini_certificate.pem" ] || \
        [ ! -e "$CERTS_LINK/server/server_cassini_key.pem" ] || \
        [ ! -e "$CERTS_LINK/client/client_cassini_certificate.pem" ] || \
        [ ! -e "$CERTS_LINK/client/client_cassini_key.pem" ]
      }

      if need_certs; then
        echo "[polar] TLS certs missing -> building .#tlsCerts"
        nix build -L .#tlsCerts -o "$CERTS_LINK"
      fi

      CA_CERT="$CERTS_LINK/ca_certificates/ca_certificate.pem"
      SERVER_CERT="$CERTS_LINK/server/server_cassini_certificate.pem"
      SERVER_KEY="$CERTS_LINK/server/server_cassini_key.pem"
      CLIENT_CERT="$CERTS_LINK/client/client_cassini_certificate.pem"
      CLIENT_KEY="$CERTS_LINK/client/client_cassini_key.pem"

      SSL_DIR="$PROJECT_ROOT/var/ssl"
      mkdir -p "$SSL_DIR"

      SERVER_CHAIN="$SSL_DIR/server_cert_chain.pem"
      cat "$SERVER_CERT" "$CA_CERT" > "$SERVER_CHAIN"

      # Cassini / Polar mTLS vars (app-specific; safe)
      : "''${TLS_CA_CERT:=$CA_CERT}"
      : "''${TLS_SERVER_CERT_CHAIN:=$SERVER_CHAIN}"
      : "''${TLS_SERVER_KEY:=$SERVER_KEY}"
      : "''${TLS_CLIENT_CERT:=$CLIENT_CERT}"
      : "''${TLS_CLIENT_KEY:=$CLIENT_KEY}"
      export TLS_CA_CERT TLS_SERVER_CERT_CHAIN TLS_SERVER_KEY TLS_CLIENT_CERT TLS_CLIENT_KEY

      : "''${BIND_ADDR:=127.0.0.1:8080}"
      : "''${CASSINI_BIND_ADDR:=$BIND_ADDR}"
      : "''${BROKER_ADDR:=$BIND_ADDR}"
      export BIND_ADDR CASSINI_BIND_ADDR BROKER_ADDR

      : "''${CONTROLLER_BIND_ADDR:=127.0.0.1:3030}"
      : "''${CONTROLLER_ADDR:=$CONTROLLER_BIND_ADDR}"
      export CONTROLLER_BIND_ADDR CONTROLLER_ADDR

      : "''${CASSINI_SERVER_NAME:=localhost}"
      : "''${CONTROLLER_SERVER_NAME:=localhost}"
      : "''${HARNESS_SERVER_NAME:=localhost}"
      export CASSINI_SERVER_NAME CONTROLLER_SERVER_NAME HARNESS_SERVER_NAME

      # -------------------------------------------------------------------
      # IMPORTANT: do NOT override SSL_CERT_FILE with the dev CA.
      # Keep system CA bundle for GitHub/Cargo/etc.
      # -------------------------------------------------------------------
      # Prefer what Nix/direnv already provides, else fall back to pkgs.cacert.
      : "''${SSL_CERT_FILE:=''${NIX_SSL_CERT_FILE:-''${SYSTEM_CERTIFICATE_PATH:-${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt}}}"
      export SSL_CERT_FILE

      # Make Cargo + git happy even if they ignore SSL_CERT_FILE in some paths
      export CARGO_HTTP_CAINFO="$SSL_CERT_FILE"
      export GIT_SSL_CAINFO="$SSL_CERT_FILE"

      # If cargo is using libgit2 and your environment/proxy/ssl gets weird,
      # force git CLI fetching (matches the error suggestion you saw).
      export CARGO_NET_GIT_FETCH_WITH_CLI=true

      pick_dns_name() {
        ${pkgs.openssl}/bin/openssl x509 -in "$SERVER_CERT" -noout -text 2>/dev/null \
          | sed -n 's/.*DNS:\([^,]*\).*/\1/p' \
          | head -n 1
      }
      DNS_NAME="$(pick_dns_name || true)"
      if [ -n "$DNS_NAME" ]; then
        echo "[polar] server cert SAN DNS (debug): $DNS_NAME"
      fi

      echo "[polar] TLS env configured:"
      echo "  TLS_CA_CERT=$TLS_CA_CERT"
      echo "  TLS_SERVER_CERT_CHAIN=$TLS_SERVER_CERT_CHAIN"
      echo "  TLS_SERVER_KEY=$TLS_SERVER_KEY"
      echo "  TLS_CLIENT_CERT=$TLS_CLIENT_CERT"
      echo "  TLS_CLIENT_KEY=$TLS_CLIENT_KEY"
      echo "  CASSINI_SERVER_NAME=$CASSINI_SERVER_NAME"
      echo "  BROKER_ADDR=$BROKER_ADDR"
      echo "  CONTROLLER_ADDR=$CONTROLLER_ADDR"
    '';
  };

}
