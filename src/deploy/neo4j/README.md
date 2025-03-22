# Dhall-to-Helm Chart

## Overview
The `generate.sh` script automates the conversion of **Dhall configuration files** into a helm chart for a neo4j deployment. It ensures:
- **Repeatable, Immutable Helm Charts** for GitOps workflows.
- **Validation of Kubernetes Manifests** using `kubectl apply --dry-run=client`.
- **Linting and Template Verification** with Helm.
- **Safe GitOps Deployments** by generating Helm artifacts that can be stored and deployed consistently.

## Prerequisites
Ensure the following tools are installed:
- **Dhall-to-YAML** (`dhall-to-yaml`): Converts Dhall configurations into Kubernetes YAML.
- **kubectl**: Validates generated Kubernetes manifests.
- **Helm**: Lints and renders the Helm chart for deployment.
- A `neo4j.conf` file to configure neo4j.
- Some kubernetes cluster for testing, such as one provisioned with `minikube`


## Known Issues

Macos Users testing using tools like minikube and podman have to deal with some additonal constraints that necessitate the
additional layers of absctraction between the podman VM and minikube container.

Unfortunately, the `minikube mount` command seems to only work for linux systems as well, so it's best to use the `minikube cp` command to copy files into the minikube container to be used using `hostPath` volumes during testing. 

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
The script will:
1. **Convert Dhall files** into Kubernetes YAML.
2. **Validate YAML** using `kubectl apply --dry-run=client`.
3. **Generate a Helm chart** in `neo4j-helm-release/`.
4. **Run Helm linting and rendering**.

## Usage
Run the script to generate a Helm chart from Dhall configurations:
```bash
chmod +x generate.sh
./generate.sh /path/to/dhall/files /path/to/chart
```
If no directory is provided, it defaults to the current directory.


### Deploying with Helm
Once the chart is generated. You can run something like

```bash
helm install polar-neo4j ./polar-neo4j -n polar --create-namespace    
```

### GitOps & Immutability
To maintain immutability and ensure **safe GitOps practices**:
1. **Commit the Helm chart to a versioned repository**:
   ```bash
   git add neo4j-helm-release/
   git commit -m "Generated immutable Helm chart from Dhall configs"
   git push origin main
   ```
2. **Use Helm package versioning** to store the chart as an immutable artifact:
   ```bash
   helm package neo4j-helm-release/
   helm push neo4j-helm-release-0.1.0.tgz oci://my-helm-repo
   ```
3. TODO: How to Deploy using GitOps tools** like ArgoCD or Flux


### Why Immutability Matters
By ensuring the Helm chart is generated **before deployment and committed to Git**, we:
- Avoid deployment drift caused by manual `helm install` changes.
- Ensure the same configuration is deployed across environments.
- Enable rollbacks to previous **known-good** Helm chart versions.
- Improve auditability and traceability of deployments.

