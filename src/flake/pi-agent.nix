# src/flake/pi-agent.nix
#
# Builds the pi_agent_rust coding agent from source.
# https://github.com/Dicklesworthstone/pi_agent_rust
#
# pi_agent_rust requires Rust nightly (2024 edition features).
# We use the same nightly toolchain already present in the polar container.
#
# The binary is named 'pi' and lands in $out/bin/pi.
# Runtime dependencies (fd, ripgrep) are already in the Dev package layer.
#
# HASHES: Run `nix build` once with the placeholder hashes, note the
# expected hashes from the error output, and replace the placeholders.

{ pkgs
, lib
, rust-bin
, fetchFromGitHub
}:

let
  # Use the same nightly toolchain as the rest of the container
  rustNightly = rust-bin.nightly.latest.default.override {
    extensions = [ "rust-src" ];
  };

in
pkgs.rustPlatform.buildRustPackage {
  pname   = "pi-agent-rust";
  version = "unstable-2026-03-16";
  doCheck = false;

  src = fetchFromGitHub {
    owner = "Dicklesworthstone";
    repo  = "pi_agent_rust";
    rev   = "a5a27a43433cfcc8ca215bb9306490522cc4279c";
    hash  = "sha256-vHCQS1vrTERfz9q3KH7QJ/3cgOWy/1sNwL3c9mf+LOA=";
  };

  cargoHash = "sha256-ePylxnZz9gTNZu/stkygim1lnd/4GFJKxW8mJB698I4=";

  nativeBuildInputs = [ rustNightly ];

  buildInputs = with pkgs; [
    openssl
    sqlite
    pkg-config
  ];

  # pi_agent_rust uses Rust 2024 edition — requires nightly
  RUSTUP_TOOLCHAIN = "nightly";

  # The binary is named 'pi'
  # Verify with: ls target/release/ | grep -v '\.d$'
  meta = with lib; {
    description = "High-performance AI coding agent CLI written in Rust";
    homepage    = "https://github.com/Dicklesworthstone/pi_agent_rust";
    license     = licenses.mit;
    platforms   = platforms.linux;
    mainProgram = "pi";
  };

  postPatch = 
    let
      modelsFile = pkgs.fetchurl {
        url  = "https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/ai/src/models.generated.ts";
        hash = "sha256-qiJQqV5X6wFoAaVih47Em5DX9AD+6jImOAyjl0pqslw=";
      };
    in ''
      mkdir -p legacy_pi_mono_code/pi-mono/packages/ai/src
      cp ${modelsFile} legacy_pi_mono_code/pi-mono/packages/ai/src/models.generated.ts
    '';
}

