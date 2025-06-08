# Common Developer Tasks for Polar
If you have `mask` installed, you can use it to quickly perform   common operations.


## build image

### dev
  > Builds the Polar Dev image. A contaienrized Rust development environment in case you don't want to do local development.
  nix build .#containers.dev
  ~~~

### polar-ci

> Builds the Polar Ci container for use in our build and testing workflows

**The CI Container**

To build the a more lean testing environment for CI/CD, we first need to download the base nix image.
This is because of limiations within the nix ecosystem. The nixpkgs dockertools don't natively support pulling images from private repositories, and nix2container, a library for managing containers with nix,
doesn't support configuring CA certificates to pull images from our private, proxied registries.
~~~sh

nix-shell -p skopeo --run "skopeo copy docker://nixos/nix:2.24.13 docker-archive:nix.tar:nix:24.13"
git add nix.tar

nix build .#containers.ciContainer -o polar-ci
# don't commit tar
git rm nix.tar
~~~

## start-dev
> Enters the Polar Dev container.

The `create-user` script will set the user within the container and then
drop into the fish shell. Replace `/path/to/your/project` with the path to
your project directory. This command mounts your project directory into the
container at the `/workspace` directory, allowing you to work on your
project files within the container.


~~~sh
podman run --rm --name polar-dev --user 0 --userns=keep-id -it -v $(pwd):/workspace:rw -p 2222:2223 polar-dev:latest bash -c "/create-user.sh $(whoami) $(id -u) $(id -g)"
~~~

## start-compose
> Starts the docker compose file to start up Polar's dependencies - Neo4J and Cassini

~~~sh
podman compose -f conf/gitlab_compose/docker-compose.yml up
~~~

## build-agents
> Builds all of the polar agents and outputs their binaries
~~~sh
nix build .#default -o polar
~~~

### cassini
> Builds the latest version of cassini and its image
~~~sh
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.cassini.cassini -o cassini
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.cassini.cassiniImage -o cassini-image
~~~

### gitlab
> builds the gitlab agents and their images
~~~sh
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.gitlabAgent.observerImage -o gitlab-observer
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.gitlabAgent.consumerImage -o gitlab-consumer
~~~
### kube
> builds the kube agents and their  images
~~~sh
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.kubeAgent.kubeObserverImage -o kube-observer
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.kubeAgent.kubeConsumerImage -o kube-consumer
~~~

## build

### polar-dev
  > Builds the Polar Dev container, see the [README](dev/README.md) for details
  ~~~sh
  nix build .#containers.devContainer
  ~~~



## render

> Uses the `render-manifests.sh` script to render all manifests in the polar/deploy repository, ensure you have the proper environment variables set.

~~~sh
sh scripts/render-manifests.sh src/deploy/polar manifests
~~~
