#!/bin/env fish

# 1. Run this from within the tests directory.

# 2. Create your env.fish file like so:

# ****************************************
# Token only needs repo read permissions, just what is needed for integration
# tests.

set -xg GITHUB_USER xxxxxxxxxxxxxx
set -xg GITHUB_TOKEN ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
# ****************************************

# 3. Ensure this file has execute permissions and run.

source ./env.fish

cargo test --test integration -- --ignored --nocapture
