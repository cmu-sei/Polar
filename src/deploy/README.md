# Polar Helm Charts

This directory contains dhall configurations to generate Helm charts for Polar services.

## Overview
The `make-chart.sh` script (located under the `scripts` folder) automates the conversion of **Dhall configuration files** into a helm chart for a deployment. It is part of the foundation of our GitOps workflow. It creates:
- **Repeatable, Immutable Helm Charts** for GitOps workflows.
- **Linting and Template Verification** with Helm.
- **Safe GitOps Deployments** by generating Helm artifacts that can be stored and deployed consistently.


### GitOps & Immutability
To maintain immutability, we commit the Helm chart to a versioned, accessed controlled git repository. All secrets are then handled per [our secrets management poilicy](../../docs/architecture/secrets-management.md).

### Why Immutability Matters
By ensuring the Helm chart is generated **before deployment and committed to Git**, we:
- Avoid deployment drift caused by manual `helm install` changes.
- Ensure the desired configuration is deployed across environments.
- Enable rollbacks to previous **known-good** Helm chart versions.
- Improve auditability and traceability of deployments.

## Tools
To accomplish this, we leverage some of the following tooling.
- **Dhall-to-YAML** (`dhall-to-yaml`): Converts Dhall configurations into Kubernetes YAML.
- **Helm**: Lints and renders the Helm chart for deployment.
- A `neo4j.conf` file to configure neo4j.
- [Minikube](https://minikube.sigs.k8s.io/docs/start/) - Initially used for local testing, feel free to use your own!
- Some client and server certificates from a trusted authroity. For testing, consider [generating your own](../agents/README.md)
- A Personal Access Token for a Gitlab instance with, at minimum, read permissions for apis, registries, and repositories.
- Container images for neo4j, cassini, and the gitlab agent should also be present. [See the documentation for info on building them](../agents/README.md). You can use your own preferred neo4j container image.
- [cert-manager ](https://cert-manager.io/docs/installation/)
- [sops](https://github.com/getsops/sops)

## Usage
We run the script to generate a Helm chart from Dhall configurations using this command in our CI

  `sh scripts/make-chart.sh src/deploy/polar polar-deploy/chart --render-templates`

## Expected Output
The make-chart script will:
1. **Convert Dhall files** into Kubernetes YAML.
2. **Generate an Umbrella Helm chart** within the given directory.
3. **Run Helm linting and rendering** to validate the chart.


### Testing with Helm
Once the chart is generated and committed, we have our immutable artifact! We can download it and run something like this to try a one-time apply.

```bash
helm install polar polar-deploy/chart -n polar --create-namespace
```

## Flux

Flux sits on the cluster constantly watching our Git repository and detects every change we make to the helm chart.
If the GitRepository or Kustomization manifests are updated, Flux will automatically pick up those changes and deploy them to the Kubernetes cluster, closing the loop to ensure continuous deployment!
