# Flake-based Docker Build Environment for Polar

This repository provides a secure, resilient, and repeatable build environment 
for Polar using NixOS and Docker. The setup ensures that the same process used 
for local development builds can also be utilized for CI/CD pipeline builds, 
providing consistency and isolation.

## Prerequisites

Before starting, ensure you have the following installed:

- [Nix package manager](https://nixos.org/download.html)
- [Docker](https://docs.docker.com/get-docker/)

## Setup

1. **Install Nix and enable it to use flakes:**
    Checout the instructions at https://nix.dev/ on how you can do this

## Building the Docker Image
1. **Enter the project directory:**

    ```bash
    cd /path/to/your/project/dev-container
    ```

2. **Build the Docker image using Nix:**

    ```bash
    nix build --extra-experimental-features nix-command --extra-experimental-features flakes
    ```

3. **Load the Docker image:**

    ```bash
    docker load < result
    ```

    The Docker image will be tagged as `polar-dev:latest`. To change the tag,
    use the Docker `tag` command specifying the new tag.

## Running the Docker Container

1. **Run the Docker container with your project directory mounted:**
    ```bash
    docker run -it -v /path/to/your/project:/workspace -p 8080:8080 polar-dev:latest bash -c "/create-user.sh $(whoami) $(id -u) $(id -g)"
    ```

    The create user command will set the user within the container and then
    drop into the fish shell. Replace `/path/to/your/project` with the path to
    your project directory. This command mounts your project directory into the
    container at the `/workspace` directory, allowing you to work on your
    project files within the container. 
    
    The `-p 8080:8080` flag forwards port 8080 from the container to your local
    machine, allowing you to access services running inside the container,
    which can be removed if not using Code Server.

## Running with VSCode Dev Containers

This setup is compatible with the VSCode Dev Containers feature, allowing you
to use Visual Studio Code as your IDE inside the Nix based container.

1. **Open this project in Visual Studio Code.**

2. **Install the Remote - Containers extension in VSCode.**

3. **Open the command palette (`Ctrl+Shift+P` or `Cmd+Shift+P`) and select
   `Dev-containers: Reopen in Container`.**


## Running Code Server

> [!NOTE]  
> You cannot run Code Server from within a container if it is already running as a VSCode Dev Container.

To run `Code Server` inside the Docker container and access it via a web browser:

1. **Start the Docker container with port forwarding:**

    ```bash
    docker run -it -v /path/to/your/project:/workspace -p 8080:8080 polar-dev:latest bash -c "/create-user.sh $(whoami) $(id -u) $(id -g)"
    ```

> ![Note]
> For Fish, please use the following command:
> ```fish
> set -xu USER_ID (id -u) && set -xu GROUP_ID (id -g) && docker run -it -v /path/to/your/project:/workspace -p 8080:8080 polar-dev:latest bash -c "/create-user.sh (whoami) $USER_ID $GROUP_ID"
> ```
    
        Replace `/path/to/your/project` with the path to your project directory.


2. **Inside the container, start `Code Server`:**

    ```bash
    code_server
    ```

3. **Access `Code Server` by navigating to `http://localhost:8080` in your web browser.**

4. [Optional if on a remote server] **Forward port via ssh:**

    ```bash
    ssh -L 8080:localhost:8080 user@remote-server
    ```

    Replace `user` with your username and `remote-server` with the IP address
    or hostname of the remote server. This command forwards port 8080 from the
    remote server to your local machine, allowing you to access `Code Server`
    running on the remote server.

### Why Use `Code Server` Over VSCode Dev Containers?

- **Fewer Dependencies:**
    Running `Code Server` does not require installing the VSCode IDE on your
    local machine, reducing the number of dependencies needed to work on your
    project, to only a web browser and docker.

- **Lightweight:**
    `Code Server` is a lightweight version of Visual Studio Code that can be
    run in a browser, making it more resource-efficient than running the full
    VSCode IDE.

- **Remote Access:**
    `Code Server` can be accessed remotely, allowing you to work on your
    project from any device with a web browser and the ability to connect to
    the server.

- **Consistency:**
    Using `Code Server` ensures that the development environment is consistent
    across different machines and setups, providing a seamless developer
    experience.


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

## Notes on update_extensions.fish
If you're tweaking the container for your development workflow, one nice thing
to do is to adjust the set of available VS Code extensions in your container.
Getting the full list of available extensions, by name and in the format that
is required by the nix package is kind of a pain, so I automated the process. I
don't think we want to store the data files in the repo, but if you run the
script, you'll get a couple of package lists that have been formatted as nix
attribute strings. These can be directly added to the flake's config for VS
Code, just search for and copy/paste the ones you want. If you don't know what
you want, it's still helpful to look for them initially in the marketplace in
the app.
