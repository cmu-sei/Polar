# Polar Helm Charts

This directory contains Helm charts for deploying Polar services for testing, please keep in mind that they are a work in progress and that they are **not ready for production use!** Expect breaking changes as we continue to improve them.

Each chart is designed to be deployed independently or as part of the larger system, although cassini and the neo4j database are currently required for operation.

## Prerequisites

Before deploying the charts, ensure you have access to the following:

- [Helm](https://helm.sh/docs/intro/install/)
- [Kubectl](https://kubernetes.io/docs/tasks/tools/)
- [Minikube](https://minikube.sigs.k8s.io/docs/start/) (Or whatever kubernetes cluster you'd like to test on)
- [Neo4j Helm Charts](https://neo4j.com/docs/operations-manual/current/kubernetes/)
- Some client and server certificates from a trusted authroity. For testing, consider [generating your own](../agents/README.md)

## Setting Up

Create some secrets to use for testing

```sh
kubectl create secret generic cassini-mtls \
    --from-file=ca_certificate.pem=/path/to/ca_certificate.pem \
    --from-file=server_polar_certificate.pem=/path/to/polar_server_certificate.pem \
    --from-file=server_polar_key.pem=/path/to/server_polar_key.pem

kubectl create secret generic client-mtls \
    --from-file=ca_certificate.pem=/path/to/ca_certificate.pem \
    --from-file=client_polar_certificate.pem=/path/to/client_polar_certificate.pem \
    --from-file=server_polar_key.pem=/path/to/client_polar_key.pem

kubectl create secret generic gitlab-secret  --from-literal=token=$GITLAB_TOKEN

```

To test the Helm charts locally, start a Minikube cluster:

```sh
# Start Minikube with enough resources
minikube start --cpus=4 --memory=8192 --driver=docker

# Verify Minikube status
minikube status

# Point kubectl to Minikube's context
kubectl config use-context minikube
```

## Deploying a Microservice

Each microservice has its own Helm chart under the `charts/` directory. To deploy a specific microservice, run:

```sh
helm install <release-name> charts/<microservice-name>
```

For example, to deploy the `cassini` broker:

```sh
helm install cassini cassini
```

## Deploying All Microservices
    TODO

## Listing Installed Releases

To see all installed Helm releases:

```sh
helm list
```

## Uninstalling a Microservice

To remove a deployed microservice:

```sh
helm uninstall <release-name>
```

```sh
helm uninstall cassini
```

## Stopping and Cleaning Up Minikube

To stop Minikube and clean up resources:

```sh
minikube stop
minikube delete
```

## Notes
- Ensure your Minikube has sufficient resources allocated.
- Modify `values.yaml` files as needed before deploying.
- Consider using `helm upgrade` to update a running deployment.

For further details, refer to the individual Helm charts under `charts/`.


