#!/bin/sh
# Render the fork platform's manifests into rendered/. The rendered YAML is committed (the exact
# objects applied are in the repository), and the operator's tests read it.
#   local/fork/render.sh
set -eu
cd "$(dirname "$0")"
mkdir -p rendered
for source in [0-9]*.dhall; do
  dhall-to-yaml --documents --file "$source" > "rendered/${source%.dhall}.yaml"
  echo "rendered/${source%.dhall}.yaml"
done
