# Polar Deployments

This directory contains dhall configurations to generate kubernetes manifests needed for Polar services.

## Overview
The `render-manifests.sh` script (located under the `scripts` folder) automates the conversion of **Dhall configuration files** into valid kubernetes manifests for deployment. It is part of the foundation of our GitOps workflow. It creates:
- **Repeatable, Immutable Manifests** for GitOps workflows.
- **Linting and Template Verification** with Helm.
- **Safe GitOps Deployments** by generating Helm artifacts that can be stored and deployed consistently.

## On Kubernetes

### GitOps & Immutability
To maintain immutability, we commit the kubernetes manifests to a versioned, accessed controlled git repository. All secrets are then handled per [our secrets management poilicy](../../docs/architecture/secrets-management.md).

[In the near-future, we'll package our manifests as OCI artifacts instead to save overhead.](https://github.com/cmu-sei/Polar/issues/77)


### Why Immutability Matters
By ensuring the kubernetes manifest is generated **before deployment and version controlled**, we:
- Avoid deployment drift caused by manual `helm install` changes.
- Ensure the desired configuration is deployed across environments.
- Enable rollbacks to previous **known-good** kubernetes manifest versions.
- Improve auditability and traceability of deployments.

## Tools
To accomplish this, we leverage some of the following tooling.
- **Dhall-to-YAML** (`dhall-to-yaml`): Converts Dhall configurations into Kubernetes YAML.
- A `neo4j.conf` file to configure neo4j.
- [Minikube](https://minikube.sigs.k8s.io/docs/start/) - Initially used for local testing, feel free to use your own!
- Some client and server certificates from a trusted authority. For testing, consider [generating your own](../agents/README.md)
- A Personal Access Token for a Gitlab instance with, at minimum, read permissions for apis, registries, and repositories.
- Container images for neo4j, cassini, and the gitlab agent should also be present. [See the documentation for info on building them](../agents/README.md). You can use your own preferred neo4j container image.
- [cert-manager ](https://cert-manager.io/docs/installation/)
- [sops](https://github.com/getsops/sops)


## Layout
We follow a pretty typical layout here, where all desired deployment environments are separated by directory.
From here,  we define a simple `types` library containing code to define types and values used across environments. We also import others.

When we want to deploy one of these environments, we run the script to generate a kubernetes manifest from Dhall configurations using this command in our CI
  `sh scripts/render-manifests.sh src/deploy/<environment> <output-dir>`

We recommend that anyone who to deploy our services take a similar approach within their own constraints.


## Flux and Continuous Deployment

Flux sits on the cluster constantly watching our Git repository and detects every change we make to the kubernetes manifests.
If the GitRepository or Kustomization manifests are updated, Flux will automatically pick up those changes and deploy them to the Kubernetes cluster, closing the loop to ensure continuous deployment!

At this time, many environment variables need to be present within our CI/CD environment.
Particularly those related to our Azure cloud environment.

Firstly, a service principal had to be created to maintain read access to our key vaults. So we need some of the following vars.

`AZURE_CLIENT_ID` – The client ID of the Azure service principal
`AZURE_TENANT_ID` – The Azure tenant ID where the application is registered.
`ACR_USERNAME` - the username associated with the ACR token
`ACR_TOKEN` – The token used to authenticate with the azure container registry so we can upload our images.
`AZURE_CLIENT_SECRET` – Token used to authenticate with azure.
`AZURE_ENVIRONMENT` - Should be "AzureUsGovernment" since that's what we're using
`AZURE_AUTHORITY_HOST` - Should point to the Azure Gov login (.us suffix)

Then there are the variables needed for actually deploying Polar's services.

`GITLAB_USER` - A username for authenticating with gitlab, particularly for flux's uses
`GITLAB_TOKEN` - A token for authenticating with gitlab.
`NEO4J_AUTH` - The default credentials for the Neo4J instance. Stored in a "username/password" foramt.
`CI_COMMIT_SHORT_SHA` - The 7 character short commit sha, used as a tag for most of our image's services by default. (This is populated automatically by Gitlab when running in CI)
`OCI_REGISTRY_AUTH` - A config.json file, stored as a string, containing credentials to one or more OCI artifact registries. This is used by the resolver agent to authenticate.

Each of Polar's services will also need environment variables of their own when deployed. See their README files for details.
