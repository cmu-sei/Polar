# Common Developer Tasks for Polar
If you have [mask](https://github.com/jacobdeichert/mask) installed, you can use it to quickly perform these common operations.
If not, consider installing it to further oxidize your life.

For example, from here in the root of the project, try running `mask build agents` to quickly build the cargo workspace with nix.
this will give you all the binaries we have defined for the project.

Our team primarily uses `podman` as a container runtime. So feel free to `alias` it for your favorite/compatible runtime.

## build-image

### dev
  > Builds the Polar Dev image. A contaienrized Rust development environment in case you don't want to do local development.
  ~~~sh
  echo "Building the Polar development environment. This may take a moment."
  nix build .#containers.devContainer --show-trace
  echo "Loading the development image."
  podman load < result
  ~~~

### cassini
  > Build's cassini's container image

  ~~~sh
  echo "building cassini image..."
  nix build .#polarPkgs.cassini.cassiniImage -o cassini
  echo "loading image..."
  podman load < result
  ~~~

### gitlab
  > builds the gitlab agents and their images
  ~~~sh
  echo "Building gitlab agent images..."
  nix build .#polarPkgs.gitlabAgent.observerImage -o gitlab-observer
  nix build .#polarPkgs.gitlabAgent.consumerImage -o gitlab-consumer
  echo "loading images..."
  podman load < gitlab-observer
  podman load < gitlab-consumer
  ~~~
### kube
  > builds the kube agents and their  images
  ~~~sh
  echo "Building kubernetes agent images..."
  nix build .#polarPkgs.kubeAgent.observerImage -o kube-observer
  nix build .#polarPkgs.kubeAgent.consumerImage -o kube-consumer
  echo "loading images..."
  podman load < kube-observer
  podman load < kube-consumer
  ~~~

## start-dev
  > Enters the Polar Dev container.

  This command mounts your project directory into the
  container at the `/workspace` directory, allowing you to work on your
  project files within the container.


  ~~~sh
  podman run --rm --name polar-dev --user 0 --userns=keep-id -it -v $(pwd):/workspace:rw -p 2222:2223 polar-dev:latest
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


### cassini
> Builds all of the polar agents and outputs their binaries
~~~sh
nix build .#polarPkgs.cassini.cassini .#polarPkgs.cassini.harnessProducer .#polarPkgs.cassini.harnessSink
~~~



## get-tls
  > Runs the nix derivation to generate TLS certificates for testing.

  ~~~sh
  nix build .#tlsCerts -o certs

  ls -alh certs
  ~~~

## render

> Uses the `render-manifests.sh` script to render all manifests in the polar/deploy repository, ensure you have the proper environment variables set.
  See [the deployment docs](src/deploy/README.md) for details.

~~~sh
sh scripts/render-manifests.sh src/deploy/polar manifests
~~~

## static-analysis
> uses the `static-tools.sh` script to run various static analysis tools on the rust source code.

~~~sh
sh scripts/static-tools.sh --manifest-path src/agents/Cargo.toml
~~~

<!--TODO: We should bring this capability back at some point-->
<!--## run-ci
> runs the `gitlab-ci.sh` script to run local ci/cd ops

~~~sh
sh scripts/gitlab-ci.sh
~~~-->
