# Common Developer Tasks for Polar
If you have `mask` installed, you can use it to quickly perform these common operations.


## build-image

### dev
  > Builds the Polar Dev image. A contaienrized Rust development environment in case you don't want to do local development.
  ~~~sh
  nix build .#containers.devContainer
  ~~~

### ci
  > Builds the Polar Dev image. A contaienrized Rust development environment in case you don't want to do local development.
  ~~~sh
  nix build .#containers.ciContainer
  ~~~

### cassini
  > Build's cassini's container image

  ~~~sh
  nix build --eval-system x86_64-linux --show-trace .#polarPkgs.cassini.cassiniImage
  podman load < result
  ~~~

### gitlab
> builds the gitlab agents and their images
~~~sh
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.gitlabAgent.observerImage -o gitlab-observer
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.gitlabAgent.consumerImage -o gitlab-consumer

podman load < gitlab-observer
podman load < gitlab-consumer
~~~
### kube
> builds the kube agents and their  images
~~~sh
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.kubeAgent.kubeObserverImage -o kube-observer
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.kubeAgent.kubeConsumerImage -o kube-consumer

podman load < kube-observer
podman load < kube-consumer
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
## start-ci
> Enters the Polar CI container.

~~~sh
podman run --rm --name polar-ci -it -v $(pwd):/workspace:rw polar-ci:latest
~~~

## start-compose
> Starts the docker compose file to start up the docker compose for local testing. Runs in background

~~~sh
podman compose -f conf/gitlab_compose/docker-compose.yml up -d
~~~

## build

### agents
> Builds all of the polar agents and outputs their binaries
~~~sh
nix build .#default -o polar
~~~

## get-tls
  > Runs the nix derivation to generate TLS certificates for testing.

  ~~~sh
  nix build .#tlsCerts -o certs

  ls -alh certs
  ~~~

## render

> Uses the `render-manifests.sh` script to render all manifests in the polar/deploy repository, ensure you have the proper environment variables set.

~~~sh
sh scripts/render-manifests.sh src/deploy/polar manifests
~~~
