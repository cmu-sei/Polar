# Polar Big Bang Sandbox Deployment

This directory contains the kubernetes manifests needed to deploy Polar services within a Big Bang enabled cluster.

## Setup

Please note there are some real bootstrapping steps that must be taken before doing a first time deployment.

This primarily surrounds setting up a few configurations for Flux to handle continuous deployment capabilities.

* The Secret needed to access the OCI repository

* The OCI Repository manifest itself.

* The Kustomize manifest to configure flux to use the OCI Repository.

The manifests for each of these are also continuously generated as part of our workflow, however it is essential that flux be configured before the first deployment.
