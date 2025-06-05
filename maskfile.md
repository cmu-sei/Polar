# Common Developer Tasks for Polar


<!-- A heading defines the command's name -->
## build-dev-enviornment

<!-- A blockquote defines the command's description -->
> Builds the Polar Dev container, see the [README](dev/README.md) for details

<!-- A code block defines the script to be executed -->
~~~sh
nix build .#devContainer
~~~

## build-ci-environment
> Builds the Polar Ci container for use in our build and testing workflows, see the [README](dev/README.md) for details

~~~sh
nix build .#ciContainer
~~~

## render

> Uses the `render-manifests.sh` script to render all manifests in the polar/deploy repository, ensure you have the proper environment variables set.

~~~sh
sh scripts/render-manifests.sh src/deploy/polar manifests
~~~
