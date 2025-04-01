# Polar Helm Charts

This directory contains dhall configurations to generate Helm charts for Polar services, please keep in mind that they are a work in progress and that they are **not ready for production use!** Expect breaking changes as we continue to improve them.

## Overview
The `make-chart.sh` script (located under the `scripts` folder) automates the conversion of **Dhall configuration files** into a helm chart for a deployment. It ensures:
- **Repeatable, Immutable Helm Charts** for GitOps workflows.
- **Linting and Template Verification** with Helm.
- **Safe GitOps Deployments** by generating Helm artifacts that can be stored and deployed consistently.

## Prerequisites
Ensure the following tools are installed:
- **Dhall-to-YAML** (`dhall-to-yaml`): Converts Dhall configurations into Kubernetes YAML.
- **Helm**: Lints and renders the Helm chart for deployment.
- A `neo4j.conf` file to configure neo4j.
- [Minikube](https://minikube.sigs.k8s.io/docs/start/) (Or whatever kubernetes cluster you'd like to test on)
- Some client and server certificates from a trusted authroity. For testing, consider [generating your own](../agents/README.md)
- Container images for neo4j, cassini, and the gitlab agent should also be present. [See the documentation for info on building them](../agents/README.md). You can use your own preferred neo4j container image.
## Setting Up

Create some resources needed to test.

```sh
kubectl create namespace polar

kubectl create secret generic cassini-mtls -n polar \
    --from-file=ca_certificate.pem=conf/certs/ca_certificates/ca_certificate.pem \
    --from-file=server_polar_certificate.pem=conf/certs/server/server_polar_certificate.pem \
    --from-file=server_polar_key.pem=conf/certs/server/server_polar_key.pem

kubectl create secret generic client-mtls -n polar \
    --from-file=ca_certificate.pem=conf/certs/ca_certificates/ca_certificate.pem \
    --from-file=client_polar_certificate.pem=conf/certs/client/client_polar_certificate.pem \
    --from-file=server_polar_key.pem=conf/certs/client/client_polar_key.pem

kubectl create secret generic gitlab-secret -n polar --from-literal=token=$GITLAB_TOKEN

# AFTER you deploy neo4j to your cluster and set a new password, you can add it as a secret for the agents to use.
kubectl create secret generic neo4j-secret -n polar --from-literal=token=$NEO4J_SECRET
```

## Known Issues

Macos Users testing using tools like minikube and podman have to deal with some additonal constraints that necessitate
additional layers of absctraction between the podman VM and minikube container.

Unfortunately, the `minikube mount` command seems to only work for linux systems, so it's best to use the `minikube cp` command to copy files into the minikube container to be used using `hostPath` volumes during testing. 

For example, to copy a configuration for neo4j to minikube's `/data` directiory, you can run

```bash
    minikube cp var/lib/neo4j/conf/neo4j.conf /data/conf/neo4j.conf      
```

It does not seem to support copying directories at this time. So some scripting may be implemented to assist this.

Finally, when exposing the neo4j service with minikube, remmeber to update the bolt port to use the port neo4j forwards for you, for example when running `minikube service -n polar neo4j --url`,

You may see output similar to the below:

```shell
http://127.0.0.1:57084 # This will be the url of your web UI
http://127.0.0.1:57085 # This will be your bolt port
❗  Because you are using a Docker driver on darwin, the terminal needs to be open to run it.
```

## Expected Output
The make-chart script will:
1. **Convert Dhall files** into Kubernetes YAML.
2. **Generate an Umbrella Helm chart** within the given directory.
3. **Run Helm linting and rendering** to validate the chart.

## Usage
Run the script to generate a Helm chart from Dhall configurations

`sh make-chart.sh dhall chart`

### Deploying with Helm
Once the chart is generated. You can run something like

```bash
helm install polar chart -n polar
```

### GitOps & Immutability
To maintain immutability and ensure **safe GitOps practices**:
1. TODO: Document how to commit the Helm chart to a versioned repository
   
2. TODO: Document how to use Helm package versioning to store the chart as an immutable artifact:

3. TODO: How to Deploy using GitOps tools** like ArgoCD or Flux


### Why Immutability Matters
By ensuring the Helm chart is generated **before deployment and committed to Git**, we:
- Avoid deployment drift caused by manual `helm install` changes.
- Ensure the same configuration is deployed across environments.
- Enable rollbacks to previous **known-good** Helm chart versions.
- Improve auditability and traceability of deployments.




