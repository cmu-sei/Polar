**NOTE:** FIXME: this readme is incomplete, many of the commands and information may be inaccurate
# Gitlab Agent

 This repository leverages [Nix flakes](https://nixos.wiki/wiki/Flakes) and the [Crane](https://github.com/ipetkov/crane) library to streamline building, testing, packaging, and linting.


Three important parts of the framework implementation include:
* GitLab Resource Observer
    * Requires credentials for Gitlab in the form of a private token configured with read and write access for the API as well as credentials for authenticating with the given rabbitmq instance. The GitLab Observer is instantiated as a set of cooperating binaries, each handling a specific type of GitLab data.
* GitLab Message Consumer (now known as the Information Processor)
    * The message consumer requires credentials for the rabbitmq instance as well and credentials for signing into a given neo4j graph database to publish information to. The information processor transforms specific GitLab data into domain-specified nodes and edges that can be used to tie CI/CD concepts to other domain knowledge.
* The Types Library
    * Contains implementation of the various GitLab types, as well as implementations  for serialization / deserialization.

All credentials, endpoints, and the like should be read in as environment variables,possibly from an environment file. There is an example an environment file in the gitlab agent [README](../../docs/README_gitlab.md) in the manual setup.

---

## Features

- **Build**: Ensures reproducible builds using Nix and Rust's `cargo` and `harkari` plugin
- **Test**: Automates running tests across the project.
- **Lint**: Enforces code quality using Clippy and other tools.
- **Package**: Produces binary packages for distribution.

---

## Prerequisites

### Tools
1. [Nix](https://nixos.org/download.html) (Ensure flakes are enabled)
2. [Rust](https://rustup.rs/) (Optional for local development)

---

## Quick Start

### Clone the Repository
```bash
git clone https://github.com/your-org/your-rust-project.git
cd your-rust-project
```

### Run All Actions with Nix
```bash
nix run .#check
```

This runs build, test, lint, and packaging steps.

---

## Using Nix Flake Commands

### Build the Project
```bash
nix build .#defaultPackage
```

### Run Tests
```bash
nix run .#test
```

### Run Linter
```bash
nix run .#lint
```

### Enter a Development Shell
```bash
nix develop
```
This sets up the environment with all necessary dependencies.

---

## Development

1. **Editing Code**  
   Use your preferred IDE (we recommend [VS Code](https://code.visualstudio.com/)) with the [rust-analyzer](https://rust-analyzer.github.io/) extension.

2. **Adding Dependencies**  
   Update the `Cargo.toml` file as usual. The Nix flake will pick them up automatically.

3. **Updating Nix Flake**  
   If new dependencies require extra system libraries, update the flake inputs accordingly in `flake.nix`.

---

## Continuous Integration (CI)

This project can integrate with CI pipelines to run `nix run .#check`, ensuring every pull request meets the required standards for builds, tests, and linting.

---

## Contributing

Contributions are welcome! Please:
1. Fork the repository.
2. Create a feature branch.
3. Submit a pull request, ensuring it passes `nix run .#check`.

---

## Troubleshooting

- **Flake errors?**  
  Ensure you have the latest Nix version and flakes enabled:
  ```bash
  nix --version
  ```

- **Rust-specific errors?**  
  Verify the toolchain version in `rust-toolchain.toml`.

---

## License

This project is licensed under the MIT License. See [LICENSE](./LICENSE) for details.

---

Thank you for contributing to the project! 🚀