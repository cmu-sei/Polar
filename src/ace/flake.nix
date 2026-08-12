{
  description = "ACE: production container build";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    pyproject-nix.url = "github:pyproject-nix/pyproject.nix";
    pyproject-nix.inputs.nixpkgs.follows = "nixpkgs";

    uv2nix.url = "github:pyproject-nix/uv2nix";
    uv2nix.inputs.nixpkgs.follows = "nixpkgs";
    uv2nix.inputs.pyproject-nix.follows = "pyproject-nix";

    pyproject-build-systems.url = "github:pyproject-nix/build-system-pkgs";
    pyproject-build-systems.inputs.nixpkgs.follows = "nixpkgs";
    pyproject-build-systems.inputs.pyproject-nix.follows = "pyproject-nix";
    pyproject-build-systems.inputs.uv2nix.follows = "uv2nix";
  };

  outputs =
    { self
    , nixpkgs
    , flake-utils
    , ...
    }@inputs:
    let
      modules = {
        python-env = import ./flake_modules/python-env.nix inputs;
        ace-viz-image = import ./flake_modules/frontend.nix;
      };
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        pyEnv = modules.python-env { inherit pkgs lib system; root = ./.; };
      in
      {
        packages = {
          "ace-viz" = modules.ace-viz-image {
            inherit pkgs lib;
            venvNoCheck = pyEnv.venvNoCheck;
            appRuntime = pyEnv.appRuntime;
          };
        };

        packages.default = self.packages.${system}."ace-viz";


        devShells.default = pkgs.mkShell {
          name = "ace-dev-shell";
          packages = [ pyEnv.venvNoCheck ];
          shellHook = ''
            export PATH="${pyEnv.venvNoCheck}/bin:$PATH"
            unset PYTHONPATH
            echo "ACE production build shell ready (Python: $(python -V))"
          '';
        };
      });
}
