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
- A Personal Access Token for a Gitlab instance with, at minimum, read permissions for apis, registries, and repositories.
- Container images for neo4j, cassini, and the gitlab agent should also be present. [See the documentation for info on building them](../agents/README.md). You can use your own preferred neo4j container image.
## Setting Up

Create some resources needed to test, in the future, these will be handled by something like the external secrets manager.

```sh
kubectl create namespace polar

kubectl create secret generic cassini-mtls -n polar \
    --from-file=ca_certificate.pem=conf/certs/ca_certificates/ca_certificate.pem \
    --from-file=server_polar_certificate.pem=conf/certs/server/server_polar_certificate.pem \
    --from-file=server_polar_key.pem=conf/certs/server/server_polar_key.pem

kubectl create secret generic client-mtls -n polar \
    --from-file=ca_certificate.pem=conf/certs/ca_certificates/ca_certificate.pem \
    --from-file=client_polar_certificate.pem=conf/certs/client/client_polar_certificate.pem \
    --from-file=client_polar_key.pem=conf/certs/client/client_polar_key.pem

kubectl create secret generic gitlab-secret -n polar --from-literal=token=$GITLAB_TOKEN

kubectl create secret generic regcred \
  --from-file=.dockerconfigjson=~/.config/containers/auth.json \
  --type=kubernetes.io/dockerconfigjson

GRAPH_USERNAME=neo4j
GRAPH_PASSWORD="somepassword"
NEO4J_AUTH="${GRAPH_USERNAME}/${GRAPH_PASSWORD}"
# Set some default credentials for neo4j in the format USERNAME/PASSWORD
kubectl create secret generic neo4j-secret -n polar-graph-db --from-literal=secret=$NEO4J_AUTH
# create a a secret for just the password to be passed to the gitlab-observer
kubectl create secret generic polar-graph-pw -n polar --from-literal=secret=$GRAPH_PASSWORD

# For those testing behind corporate proxies, create a secret for your proxy CA
kubectl create secret generic proxy-ca-cert -n polar \
    --from-file=proxy.crt=conf/certs/host/zscaler.pem
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

**Container Images**
Tools like minikube and kind require images to be accessible directly by worker nodes. This means that commands like `minikube image load localhost/cassini:0.1.0` need to be ran before minikube can use it.


**On file permissions**
If you're using hostPath volumes, you'll need to be sure that neo4j's default user `7474` can read and write to the given path. For example `/data/neo4j/` on the host.

You can do this using `chown -R 7474:7474 /data`

**Exposing Services**

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

`sh scripts/make-chart.sh polar polar-deploy/chart`

### Deploying with Helm
Once the chart is generated. You can run something like

```bash
helm install polar polar-deploy/chart -n polar --create-namespace
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




