# Common Developer Tasks for Polar
If you have `mask` installed, you can use it to quickly alias these various common operations.

## build
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

### gitlab-agent
> builds the gitlab agents and their images
~~~sh
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.gitlabAgent.observerImage -o gitlab-observer
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.gitlabAgent.consumerImage -o gitlab-consumer
~~~
### kube-agent
> builds the kube agents and their  images
~~~sh
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.kubeAgent.kubeObserverImage -o kube-observer
nix build --eval-system x86_64-linux --show-trace .#polarPkgs.kubeAgent.kubeConsumerImage -o kube-consumer
~~~

<!-- TODO: Uncomment when we unify the containers module -->
<!-- ### dev-env
  > Builds the Polar Dev container, see the [README](dev/README.md) for details
  ~~~sh
  nix build .#containers.dev
  ~~~ -->
<!-- ## build-ci-environment
> Builds the Polar Ci container for use in our build and testing workflows, see the [README](dev/README.md) for details
~~~sh
nix build .#containers.ci
~~~ -->
## start-dev
> Enters the Polar Dev container, see the [README](dev/README.md) for details
~~~sh
podman run --rm --name polar-dev --user 0 --userns=keep-id -it -v $(pwd):/workspace:rw -p 2222:2223 polar-dev:latest bash -c "/create-user.sh $(whoami) $(id -u) $(id -g)"
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
