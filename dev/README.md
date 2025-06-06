# Flake-based Docker Build Environment for Polar

This repository provides secure, resilient, and repeatable build environments
for Polar using NixOS and Docker. The setup ensures that the same process used
for local development builds can also be utilized for CI/CD pipeline builds,
providing consistency and isolation.

## Prerequisites

Before starting, ensure you have the following installed:

- [Nix package manager](https://nixos.org/download.html)
- [Docker](https://docs.docker.com/get-docker/)

## Setup

1. **Install Nix and enable it to use flakes:**
    Checkout the instructions at https://nix.dev/ on how you can do this.
    Chances are you can run the following command once you have nix installed to configure it
    ```sh
    printf 'experimental-features = nix-command flakes' > "$HOME/.config/nix/nix.conf"
    ```

## Building the Docker Images
1. **Enter the project directory:**

    ```bash
    cd /path/to/your/project/dev-container
    ```

2. **Build one of the images using Nix:**

    ```bash
    # Builds a full rust development environment for the project
    nix build .#devContainer
    ```

    **The CI Container**

    To build the CI/CD container for a more lean testing environment, we first need to download the base nix image.

    ```sh
    skopeo copy docker://nixos/nix:2.24.13 docker-archive:nix.tar:nix:24.13
    ```

    This is because of limiations within the nix ecosystem. The nixpkgs dockertools don't natively sujpport pulling images from private repositories, and nix2container, a library for managing containers with nix,
    doesn't support configuring CA certificates to pull images from our private, proxied registries.

    (At least, not at time of writing!)

    So we're kindof stuck when working in more regulated environments.

    Once you have the nix image downloaded and present as a tarfile, you can run the build command for the derivation.

    nix build .#ciContainer

3. **Load the Docker image:**

    ```bash
    docker load < result
    ```

    The development image will be tagged as `polar-dev:latest`. To change the tag,
    use the Docker `tag` command specifying the new tag.

## Running the Docker Container

1. **Run the Docker containers with your project directory mounted:**
    ```bash
    docker run -it -v /path/to/your/project:/workspace -p 2222:2222 polar-dev:latest bash -c "/create-user.sh $(whoami) $(id -u) $(id -g)"
    ```

    The create user command will set the user within the container and then
    drop into the fish shell. Replace `/path/to/your/project` with the path to
    your project directory. This command mounts your project directory into the
    container at the `/workspace` directory, allowing you to work on your
    project files within the container.

## Running with VSCode Dev Containers

This setup is compatible with the VSCode Dev Containers feature, allowing you
to use Visual Studio Code as your IDE inside the Nix based container.

1. **Open this project in Visual Studio Code.**

2. **Install the Remote - Containers extension in VSCode.**

3. **Open the command palette (`Ctrl+Shift+P` or `Cmd+Shift+P`) and select
   `Dev-containers: Reopen in Container`.**


## Benefits of Using Nix and Flakes

**Security and Reproducibility:**
Nix provides a highly reproducible build system by describing the entire build
environment as code, ensuring that builds are consistent across different
environments and over time. This reduces the "works on my machine" problems and
enhances security by eliminating unpredictable states. Nix Flakes further
secure the process by locking down dependency versions and providing an
isolated, declarative approach to package management.

**Compatibility:**
The use of Nix Flakes makes this environment easily compatible with VSCode Dev
Containers, ensuring a seamless developer experience across different machines
and setups.

**Efficiency:**
The Nix-based environment is lightweight and efficient, by only installing the
necessary dependencies for the build process, reducing the overall size and
complexity of the build environment and speeding up the build process.
