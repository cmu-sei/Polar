{
system,
pkgs,
rust-overlay,
staticanalysis,
dotacat,
myNeovimOverlay,
}:

let

    packageSets = import ./packages.nix { inherit system pkgs rust-overlay staticanalysis dotacat; };

    # ---------------------------------------------------------------------
    #  Files required by dev-containers.
    #  Sets up users and permissions.
    # ---------------------------------------------------------------------

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

    etcShadow    = pkgs.writeTextDir "etc/shadow"     shadowEntries;
    etcPasswd    = pkgs.writeTextDir "etc/passwd"     passwdEntries;
    etcGroup     = pkgs.writeTextDir "etc/group"      groupEntries;
    etcGshadow   = pkgs.writeTextDir "etc/gshadow"    gshadowEntries;
    etcShells    = pkgs.writeTextDir "etc/shells"     shellsFile;
    etcOsRelease = pkgs.writeTextDir "etc/os-release" osRelease;

    baseInfo = [
      etcShadow
      etcPasswd
      etcGroup
      etcGshadow
      etcShells
      etcOsRelease
    ];

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
    # Fish vendor functions (system-wide)
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
           clean=$(basename "$f" | sed -E 's/^[0-9a-z]{32,}-//')
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

    ldLibraryPath = pkgs.lib.makeLibraryPath [ devEnv pkgs.stdenv.cc.cc.lib ];

    interactiveShellInitFile = pkgs.writeTextFile {
      name        = "interactiveShellInit.fish";
      destination = "/etc/fish/interactiveShellInit.fish";
      # PORTABILITY: interactiveShellInit is a derivation evaluated in the
      # target-arch context, so embedding store paths here via string
      # interpolation is safe — the hashes will be correct for the arch being
      # built. This is intentionally different from config.Env, which is
      # evaluated once on the host and must never contain store paths.
      text        = import ./container-files/shell/fish/interactiveShellInit.nix {
        inherit fisheyGrc bass bobthefish starshipBin atuinBin editor fishShell;
      } + ''

        # LD_LIBRARY_PATH is set only in interactive shells so that Nix
        # commands (which carry correct rpaths) are not broken by library
        # overrides in non-interactive contexts.
        set -gx LD_LIBRARY_PATH "${ldLibraryPath}"
      '';
    };

    # ---------------------------------------------------------------------
    # Portable derivations replacing extraCommands / runAsRoot
    # ---------------------------------------------------------------------

    # PORTABILITY FIX: previously symlinked to pkgs.stdenv.cc.cc (the GCC
    # compiler runtime), which does not contain the glibc dynamic linker.
    # The correct source is pkgs.glibc, which owns ld-linux-*.so. On x86
    # the old target happened to resolve because of how the host stdenv was
    # assembled, masking the bug on aarch64.
    ldLinker = let
      linkerName = if system == "x86_64-linux"
        then "ld-linux-x86-64.so.2"
        else if system == "aarch64-linux"
        then "ld-linux-aarch64.so.1"
        else throw "unsupported system: ${system}";
      linkerDir = if system == "x86_64-linux" then "lib64" else "lib";
    in pkgs.runCommand "ld-linker" {} ''
      mkdir -p $out/${linkerDir}
      ln -sf ${pkgs.glibc}/lib/${linkerName} $out/${linkerDir}/${linkerName}
    '';

    usrBinEnv = pkgs.runCommand "usr-bin-env" {} ''
      mkdir -p $out/usr/bin
      ln -s ${pkgs.coreutils}/bin/env $out/usr/bin/env
    '';

    fhsDirs = pkgs.runCommand "fhs-dirs" {} ''
      mkdir -p $out/var/tmp
      mkdir -p $out/tmp
    '';

    # PORTABILITY FIX: startScript and polarHelpScript are now included in
    # devEnv via buildEnv so that /bin/start.sh and /bin/polar-help are
    # stable FHS-rooted paths. Previously, startScript was added to image
    # contents as a bare derivation and referenced via its store path in
    # config.Cmd — that store path is evaluated once on the build host and
    # does not match the arch-specific hash produced when building the arm64
    # image, causing Cmd to point to a path that does not exist in the
    # arm64 image layers.
    polarHelpScript = pkgs.writeShellScriptBin "polar-help" (
      builtins.readFile ./container-files/polar-help
    );

    # PORTABILITY FIX: runtime env vars that require store paths (fish plugin
    # paths, LIBCLANG_PATH, LOCALE_ARCHIVE, COREUTILS, etc.) have been moved
    # here from config.Env into the start script body. start.sh is a
    # derivation evaluated in the target-arch context, so its interpolated
    # store paths are always correct for the arch being built. config.Env is
    # evaluated once on the host and its interpolated store paths would be
    # wrong on any architecture other than the build host.
    startScript = pkgs.writeShellScriptBin "start.sh" (
      # Prepend arch-correct store-path exports, then append the static body.
      ''
        export BOB_THE_FISH="${pkgs.fishPlugins.bobthefish}"
        export FISH_BASS="${pkgs.fishPlugins.bass}"
        export FISH_GRC="${pkgs.fishPlugins.grc}"
        export LIBCLANG_PATH="${pkgs.llvmPackages_19.libclang.lib}/lib"
        export LOCALE_ARCHIVE="${pkgs.glibcLocalesUtf8}/lib/locale/locale-archive"
        export COREUTILS="${pkgs.uutils-coreutils-noprefix}"
      ''
      + builtins.readFile ./container-files/start.sh
    );

    # ---------------------------------------------------------------------
    # Dev/CI envs
    # PORTABILITY FIX: startScript and polarHelpScript are now part of
    # devEnv so their binaries land at /bin/start.sh and /bin/polar-help
    # via buildEnv's symlink tree. config.Cmd and config.Env can then
    # reference these as stable /bin/* paths with no store-path dependency.
    # ---------------------------------------------------------------------
    devEnv = pkgs.buildEnv {
        name = "dev-env";
        paths = packageSets.devPkgs ++ [ startScript polarHelpScript ];
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

    # PORTABILITY FIX: closureInfo previously only listed devEnv as a root,
    # leaving all other image contents (scripts, config files, vendor funcs,
    # etc.) unregistered in the Nix DB. Any in-container nix-store query or
    # GC operation would treat those paths as invalid. All derivations that
    # appear in image contents are now listed as roots so the registration
    # is complete.
    #
    # NOTE: nixDbRegistration is intentionally excluded from this list.
    # nixDbRegistration depends on closureInfo (it reads closureInfo/registration
    # at build time), so including it here would create a cycle:
    #   closureInfo -> nixDbRegistration -> closureInfo
    # nixDbRegistration itself has no store paths that need to be registered
    # inside the container — it is a build-time tool, not a runtime artifact.
    closureInfo = pkgs.closureInfo {
      rootPaths = [
        devEnv
        fishConfig
        gitconfig
        interactiveShellInitFile
        license
        nixConfig
        nixRegistry
        shellInitFile
        containerPolicyConfig
        vendorFuncs
        ldLinker
        usrBinEnv
        fhsDirs
        etcShadow
        etcPasswd
        etcGroup
        etcGshadow
        etcShells
        etcOsRelease
      ];
    };

    nixDbRegistration = pkgs.runCommand "nix-db-registration" {} ''
      mkdir -p $out/nix/var/nix/db
      export NIX_REMOTE=local?root=$out
      ${pkgs.nix}/bin/nix-store --load-db < ${closureInfo}/registration
    '';

    # Create GC roots baked into the image that protect the entire container
    # closure from nix-collect-garbage. Without this, running nix-collect-garbage
    # inside the container destroys the container's own environment — the Nix
    # daemon sees no live roots pointing at the image's store paths and deletes
    # them all.
    #
    # Each symlink points directly at a top-level derivation output. Nix follows
    # the reference graph from each root and protects the full transitive closure.
    #
    # IMPORTANT: GC root symlinks must point at store paths directly — NOT at
    # closureInfo or nixDbRegistration. Those are themselves store paths, so a
    # symlink pointing at them is "invalid" from Nix's perspective (a root inside
    # the store pointing at another path inside the store provides no anchor).
    # Only symlinks that originate from outside the store (e.g. /nix/var/nix/gcroots/)
    # and point at store paths are treated as live roots.
    #
    # NOTE: gcRoots itself is intentionally excluded from closureInfo's rootPaths
    # to avoid a cycle. Its own store path is protected by being listed in image
    # contents, which the Nix DB registration covers.
    gcRoots = pkgs.runCommand "gc-roots" {} ''
      mkdir -p $out/nix/var/nix/gcroots
      ln -s ${devEnv}                   $out/nix/var/nix/gcroots/polar-dev-env
      ln -s ${fishConfig}               $out/nix/var/nix/gcroots/polar-fish-config
      ln -s ${interactiveShellInitFile} $out/nix/var/nix/gcroots/polar-interactive-init
      ln -s ${vendorFuncs}              $out/nix/var/nix/gcroots/polar-vendor-funcs
      ln -s ${nixConfig}                $out/nix/var/nix/gcroots/polar-nix-config
      ln -s ${nixRegistry}              $out/nix/var/nix/gcroots/polar-nix-registry
      ln -s ${shellInitFile}            $out/nix/var/nix/gcroots/polar-shell-init
      ln -s ${containerPolicyConfig}    $out/nix/var/nix/gcroots/polar-policy
      ln -s ${ldLinker}                 $out/nix/var/nix/gcroots/polar-ld-linker
      ln -s ${usrBinEnv}                $out/nix/var/nix/gcroots/polar-usr-bin-env
      ln -s ${fhsDirs}                  $out/nix/var/nix/gcroots/polar-fhs-dirs
      ln -s ${gitconfig}                $out/nix/var/nix/gcroots/polar-gitconfig
      ln -s ${license}                  $out/nix/var/nix/gcroots/polar-license
      ln -s ${etcShadow}                $out/nix/var/nix/gcroots/polar-etc-shadow
      ln -s ${etcPasswd}                $out/nix/var/nix/gcroots/polar-etc-passwd
      ln -s ${etcGroup}                 $out/nix/var/nix/gcroots/polar-etc-group
      ln -s ${etcGshadow}               $out/nix/var/nix/gcroots/polar-etc-gshadow
      ln -s ${etcShells}                $out/nix/var/nix/gcroots/polar-etc-shells
      ln -s ${etcOsRelease}             $out/nix/var/nix/gcroots/polar-etc-os-release
    '';

    # Nix flake registry pointing at the exact nixpkgs revision the container
    # was built against. This allows users to run 'nix search nixpkgs',
    # 'nix shell nixpkgs#foo', and 'nix run nixpkgs#foo' without network
    # access and without drift — they get the same nixpkgs tree that produced
    # the container, already present in the store.
    #
    # pkgs.path is the store path of the nixpkgs input as resolved by the
    # flake.lock at build time. Using a path reference rather than a github
    # reference means the registry resolves locally with no fetch required,
    # and is consistent with everything else in the image.
    nixRegistry = pkgs.writeTextFile {
      name        = "registry.json";
      destination = "/etc/nix/registry.json";
      text        = builtins.toJSON {
        version = 2;
        flakes  = [{
          from = { type = "indirect"; id = "nixpkgs"; };
          to   = { type = "path"; path = "${pkgs.path}"; };
        }];
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

    # ---------------------------------------------------------------------
    # Dev container image (layered)
    # ---------------------------------------------------------------------
    devContainer = pkgs.dockerTools.buildLayeredImage {
      name = "polar-dev";
      tag  = "latest";
      maxLayers = 100;

      # PORTABILITY: startScript and polarHelpScript have been removed from
      # contents. They now live inside devEnv (see above). Adding them here
      # as bare derivations previously caused their store paths to be
      # referenced directly in config.Cmd, which is evaluated on the build
      # host and produces arch-incorrect hashes for cross-arch builds.
      contents = baseInfo ++ [
        devEnv
        fishConfig
        gitconfig
        interactiveShellInitFile
        license
        nixConfig
        nixRegistry
        shellInitFile
        containerPolicyConfig
        vendorFuncs
        ldLinker
        usrBinEnv
        nixDbRegistration
        gcRoots
        fhsDirs
      ];

      config = {
        WorkingDir = "/workspace";
        Env = [
          # PORTABILITY FIX: All store-path-bearing env vars (BOB_THE_FISH,
          # FISH_BASS, FISH_GRC, LIBCLANG_PATH, LOCALE_ARCHIVE, COREUTILS)
          # have been removed from here and moved into start.sh (see
          # startScript above). config.Env is evaluated once on the build
          # host; any store path interpolated here will be an x86 hash even
          # when building the arm64 image. start.sh is a derivation and is
          # evaluated in the target-arch context, so it is the correct place
          # for arch-sensitive store path exports.

          # PORTABILITY FIX: PATH no longer contains ${devEnv}/bin (a store
          # path) and no longer expands $PATH from the host environment.
          # buildEnv symlinks all binaries into /bin, so /bin is sufficient.
          # $PATH in config.Env is NOT expanded from a prior ENV layer the
          # way a Dockerfile ENV instruction would be — it is taken from the
          # host shell at image-build time, which is both arch-incorrect and
          # non-deterministic.
          "PATH=/bin:/usr/bin:/root/.cargo/bin"

          "CC=clang"
          "CXX=clang++"
          "CMAKE=/bin/cmake"
          "CMAKE_MAKE_PROGRAM=/bin/make"
          "LANG=en_US.UTF-8"
          "TZ=UTC"
          "MANPAGER=sh -c 'col -bx | bat --language man --style plain'"
          "MANPATH=/share/man"
          "RUSTFLAGS=-Clinker=clang-lld-wrapper"
          "PKG_CONFIG_PATH=/lib/pkgconfig"
          "SHELL=/bin/fish"
          "SSL_CERT_DIR=/etc/ssl/certs"
          "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
          "CARGO_HTTP_CAINFO=/etc/ssl/certs/ca-bundle.crt"
          "USER=root"
        ];
        Volumes = {};
        # PORTABILITY FIX: previously "${startScript}/bin/start.sh", which
        # is a store path baked in at flake-evaluation time on the build
        # host. On aarch64 the store hash for startScript differs from the
        # x86 hash captured here, so the Cmd pointed to a path absent from
        # the arm64 image layers. /bin/start.sh is stable because buildEnv
        # symlinks startScript's bin/ into /bin.
        Cmd = [ "/bin/start.sh" ];
      };
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

      # NOTE: store-path exports above (OPENSSL_DIR, LIBCLANG_PATH, etc.)
      # are safe here because mkShell.shellHook is evaluated in the
      # target-arch nix develop context, not at flake-evaluation time on
      # the build host. These are dev-shell-only and never flow into image
      # config.

      # -------------------------------------------------------------------
      # Polar TLS auto-setup (direnv/nix develop)
      # PORTABILITY NOTE: set -euo pipefail is intentionally NOT used here.
      # shellHook runs in the user's interactive shell; set -e will cause
      # the entire shell session to exit on any non-zero return, including
      # innocuous operations (e.g. grep with no matches, missing optionals).
      # Errors are handled explicitly below instead.
      # -------------------------------------------------------------------

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
        # Explicit error message rather than relying on set -e to surface
        # failure, so the developer gets a clear actionable message.
        nix build -L .#tlsCerts -o "$CERTS_LINK" || {
          echo "[polar] ERROR: failed to build TLS certs. Check nix build output above."
          return 1
        }
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

      export CASSINI_SHUTDOWN_TOKEN=HEYTHERE

      export TMPDIR=$(mktemp -d)

      export JAEGER_ENABLE_TRACING=1
      export JAEGER_OTLP_ENDPOINT=http://localhost:4318/v1/traces

      export RUST_LOG=trace

      export GRAPH_DB="neo4j"
      export GRAPH_ENDPOINT="bolt://127.0.0.1:7687"
      export GRAPH_PASSWORD="somepassword"
      export GRAPH_USER="neo4j"

      export POLAR_SCHEDULER_LOCAL_PATH="/home/djshepard/Documents/projects/polar-sched-test"
      export POLAR_SCHEDULER_REMOTE_URL="https://github.com/daveman1010221/polar-schedules.git"
      export POLAR_SCHEDULER_SYNC_INTERVAL="120"
      export POLAR_SCHEDULER_GIT_USERNAME=""
      export POLAR_SCHEDULER_GIT_PASSWORD=""

      : "''${SSL_CERT_FILE:=''${NIX_SSL_CERT_FILE:-''${SYSTEM_CERTIFICATE_PATH:-${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt}}}"
      export SSL_CERT_FILE

      export CARGO_HTTP_CAINFO="$SSL_CERT_FILE"
      export GIT_SSL_CAINFO="$SSL_CERT_FILE"
      export CARGO_NET_GIT_FETCH_WITH_CLI=true

      # PORTABILITY NOTE: pick_dns_name is informational only. The || true
      # guard is intentional — cert may not exist yet on first run, and a
      # missing SAN is not a fatal condition for shell init. The explicit
      # error handling on nix build above is where we actually gate on
      # cert presence.
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
