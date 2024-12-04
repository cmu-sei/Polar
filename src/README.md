# Rust Project with Nix Flake Support 

Welcome to this Rust project! This repository leverages [Nix flakes](https://nixos.wiki/wiki/Flakes) and the [Crane](https://github.com/ipetkov/crane) library to streamline building, testing, packaging, and linting.

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