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
    # define a fixed number of build users;
    # TODO: Make mroe dynamic? We make one per available CPU in the create-user script
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

    # skeleton file for create-user.sh to copy
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

    # User creation script
    createUserScript = pkgs.writeTextFile {
      name        = "create-user.sh";
      destination = "/create-user.sh";
      text        = builtins.readFile ./container-files/create-user.sh;
      executable  = true;
    };

    # ---------------------------------------------------------------------
    # Dev container image
    # ---------------------------------------------------------------------
    devContainer = pkgs.dockerTools.buildImage {
      name = "polar-dev";
      tag  = "latest";
      copyToRoot = [
        baseInfo
        createUserScript
        devEnv
        fishConfig
        gitconfig
        interactiveShellInitFile
        license
        nixConfig
        shellInitFile
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

          "LANG=en_US.UTF-8"
          "TZ=UTC"
          "MANPAGER=sh -c 'col -bx | bat --language man --style plain'"
          "MANPATH=${pkgs.man-db}/share/man:$MANPATH"
          "LOCALE_ARCHIVE=${pkgs.glibcLocalesUtf8}/lib/locale/locale-archive"

          # stdenv.cc is clang-based now, so this is fine:
          "LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [ devEnv pkgs.stdenv.cc.cc.lib ]}"

          #"LIBCLANG_PATH=${pkgs.libclang.lib}/lib/"

          "RUSTFLAGS=-Clinker=clang-lld-wrapper"

          "PATH=$PATH:/bin:/usr/bin:${devEnv}/bin:/root/.cargo/bin"

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
        Cmd = [ "/bin/fish" ];
      };
      extraCommands = ''
        # Link the env binary (needed for the check requirements script)
        mkdir -p usr/bin
        ln -s ${pkgs.coreutils}/bin/env usr/bin/env

        # Link the dynamic linker/loader (needed for Node)
        mkdir -p lib64
        ln -s ${pkgs.stdenv.cc.cc}/lib/ld-linux-x86-64.so.2 lib64/

        mkdir -p tmp
      '';
    };

    ciContainer = pkgs.dockerTools.buildImage {
      name = "polar-ci";
      tag  = "latest";
      copyToRoot = [
        ciEnv
        baseInfo
        gitconfig
        license
        nixConfig
        containerPolicyConfig
      ];
      config = {
        WorkingDir = "/workspace";
        Env = [
          "COREUTILS=${pkgs.uutils-coreutils-noprefix}"
          "LANG=en_US.UTF-8"
          "TZ=UTC"
          "MANPAGER=sh -c 'col -bx | bat --language man --style plain'"
          "MANPATH=${pkgs.man-db}/share/man:$MANPATH"
          "LOCALE_ARCHIVE=${pkgs.glibcLocalesUtf8}/lib/locale/locale-archive"

          "PATH=$PATH:/bin:/usr/bin:${ciEnv}/bin:/root/.cargo/bin"
          "SSL_CERT_DIR=/etc/ssl/certs"
          # Add certificates to allow for cargo to download files from the
          # internet. May have to adjust this for FIPS.
          "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
          "USER=root"
        ];
        Volumes = {};
        Cmd = [ "/bin/bash" ];
      };
      extraCommands = ''
        # Link the env binary (needed for the check requirements script)
        mkdir -p usr/bin
        ln -s ${pkgs.coreutils}/bin/env usr/bin/env
        mkdir -p tmp
      '';
    };




in {
  inherit devContainer ciContainer;

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

}
