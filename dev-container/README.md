# Flake-based Docker Build Environment for Polar

This repository provides a secure, resilient, and repeatable build environment for Polar using NixOS and Docker. The setup ensures that the same process used for local development builds can also be utilized for CI/CD pipeline builds, providing consistency and isolation.

## Prerequisites

Before starting, ensure you have the following installed:

- [Nix package manager](https://nixos.org/download.html)
- [Docker](https://docs.docker.com/get-docker/)

## Setup

1. **Install NixOS:**

    Follow the [NixOS installation guide](https://nixos.org/manual/nixos/stable/#ch-installation) to install NixOS on your system.

## Building the Docker Image
1. **Enter the project directory:**

    ```bash
    cd /path/to/your/project/dev-container
    ```

2. **Build the Docker image using Nix:**

    ```bash
    nix build --extra-experimental-features nix-command --extra-experimental-features flakes
    ```

3. **Load the Docker image into Docker:**

    ```bash
    docker load < result
    ```

    The Docker image will be tagged as `polar-dev:latest`. To change the tag, use the Docker `tag` command specifying the new tag.

## Running the Docker Container

1. **Run the Docker container with your project directory mounted:**
    ```bash
    docker run -it -v /path/to/your/project:/workspace polar-dev:latest bash -c "/create_user.sh $(whoami) $(id -u) $(id -g)"
    ```

    The create user command will set the user within the container and then drop into the fish shell. Replace `/path/to/your/project` with the path to your project directory. This command mounts your project directory into the container at the `/workspace` directory, allowing you to work on your project files within the container.

## Running with VSCode Dev Containers

This setup is compatible with the VSCode Dev Containers feature, allowing you to use Visual Studio Code as your IDE inside the Nix based container.

1. **Open this project in Visual Studio Code.**

2. **Install the Remote - Containers extension in VSCode.**

3. **Open the command palette (`Ctrl+Shift+P`) and select `Dev-containers: Reopen in Container`.**


## Running Code Server

To run `code-server` inside the Docker container and access it via a web browser:

1. **Start the Docker container with port forwarding:**

    ```bash
    docker run -it -p 8080:8080 -v /path/to/your/project:/workspace polar-dev:latest
    ```

2. **Inside the container, start `code-server`:**

    ```bash
    code-server --bind-addr 0.0.0.0:8080 /workspace
    ```

3. **Access `code-server` by navigating to `http://localhost:8080` in your web browser.**

### Why Use `code-server` Over VSCode Dev Containers?

- **Remote Access:** `code-server` allows you to access your development environment from any device with a web browser, enabling remote development without the need for a local VSCode installation.
  
- **Lightweight Setup:** Running `code-server` in a Docker container can be more lightweight and resource-efficient compared to running the full VSCode Dev Containers setup.

- **Consistency:** `code-server` provides a consistent development environment regardless of the local machine setup, ensuring that all developers work with the same tools and configurations.

- **Security:** `code-server` runs in an isolated Docker container, providing an additional layer of security by keeping the development environment separate from the host system. This isolation helps protect the host from potential security vulnerabilities within the development environment.

## Benefits of Using Nix and Flakes

**Security and Reproducibility:**

Nix provides a highly reproducible build system by describing the entire build environment as code, ensuring that builds are consistent across different environments and over time. This reduces the "works on my machine" problems and enhances security by eliminating unpredictable states. Nix Flakes further secure the process by locking down dependency versions and providing an isolated, declarative approach to package management.

**Compatibility:**

The use of Nix Flakes makes this environment easily compatible with VSCode Dev Containers, ensuring a seamless developer experience across different machines and setups.
