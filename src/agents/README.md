# Polar Agents

This directory contains several of the agents that compose the polar framework as well as a common library.

## Building
The workspace leverages [Nix flakes](https://nixos.wiki/wiki/Flakes) and the [Crane](https://github.com/ipetkov/crane) library to streamline building, testing, packaging, and linting. Ensure the nix package manager is installed with flakes enabled to take advantage of it.


To build the project agents, you can simply run

`nix build` from this directory 


To build individual or multiple components

```sh
## Buiilds the cassini message broker and its dependnecies
nix build .#cassiniImage

# Build only the observer or consumer agents
nix build .#gitlabObserver

nix build .#gitlabConsumer

# build multiple images
nix build .#observerImage .#consumerImage

# Run static analysis, unit tests via derivations in checks
nix flake check

### For the impure - Build on a remote host
nix build \
--eval-system x86_64-linux \
--system x86_64-linux \
--builders "ssh://user@some-linux-host x86_64-linux" .#packages.x86_64-linux.default \
--show-trace \
--impure         

# Impure remote checks
# nix flake check --eval-system x86_64-linux --builders "ssh://user@some-linux-host x86_64-linux" --show-trace --impure   

```

For additional information on using flakes please see the documentation.