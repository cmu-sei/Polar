# Common Developer Tasks for Polar
If you have `mask` installed, you can use it to quickly alias these various common operations.
<!-- A heading defines the command's name -->
## build-dev-enviornment

<!-- A blockquote defines the command's description -->
> Builds the Polar Dev container, see the [README](dev/README.md) for details

<!-- A code block defines the script to be executed -->
~~~sh
nix build .#devContainer
~~~

## start-dev
<!-- A blockquote defines the command's description -->
> Enters the Polar Dev container, see the [README](dev/README.md) for details

~~~sh
podman run --rm --name polar-dev --user 0 --userns=keep-id -it -v $(pwd):/workspace:rw -p 2222:2223 polar-dev:latest bash -c "/create-user.sh $(whoami) $(id -u) $(id -g)"
~~~
## build-ci-environment
> Builds the Polar Ci container for use in our build and testing workflows, see the [README](dev/README.md) for details

~~~sh
nix build .#ciContainer
~~~

## build-impure
> Builds the Polar Ci container for use in our build and testing workflows, see the [README](dev/README.md) for details

~~~sh
nix build --impure --eval-system x86_64-linux .#packages.x86_64-linux.default
~~~

## start-compose
> Starts the docker compose file to start up Polar's dependencies - Neo4J and Cassini

~~~sh
podman compose -f conf/gitlab_compose/docker-compose.yml up 
~~~
## render

> Uses the `render-manifests.sh` script to render all manifests in the polar/deploy repository, ensure you have the proper environment variables set.

~~~sh
sh scripts/render-manifests.sh src/deploy/polar manifests
~~~
