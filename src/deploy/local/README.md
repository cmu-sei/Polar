
# Local (Minikube) Deployment Configurations (Dhall)

This directory contains the Dhall sources used to generate Kubernetes manifests for a local Minikube environment. The goal is reproducible configuration expressed in Dhall’s type system, with zero template magic and no silent defaults. Everything here is meant to be deliberate and inspectable.

The directory is structured around a few expectations:

* Each Kubernetes object is defined as a Dhall expression returning a single, strongly-typed resource.
* Nothing in this directory performs external I/O. Dhall does not generate base64, JSON, or derived artifacts. CI does that.
* Every rendered manifest is meant to be fed directly into `kubectl` without preprocessing.

## Minikube Setup

You need Minikube installed and a functioning local Docker environment. Start Minikube with enough resources to run whatever you're deploying:

```
minikube start \
  --cpus=4 \
  --memory=8g \
  --driver=podman # or docker if you prefer
```

If you rely on container images built locally, point Minikube at the host Docker daemon:
Note that if you want to pull from any remote repos, you'll have to add Image pull secrets.

```
eval $(minikube docker-env)
```

This prevents you from having to push images to a registry just to test them locally.

## Rendering Dhall → YAML

Dhall expressions in this directory intentionally produce typed Kubernetes objects. You render each file using `dhall-to-yaml`:

```
dhall-to-yaml --file ./deployment.dhall > deployment.yaml
```

Or recursively:

```
find . -name '*.dhall' -maxdepth 1 \
  -exec sh -c 'dhall-to-yaml --file "$1" > "${1%.dhall}.yaml"' _ {} \;
```

If your Dhall files import environment variables (e.g. secrets), make sure they are exported prior to rendering or the process will fail loudly.

## Applying the Manifests

Once rendered, deploy them into Minikube:

```
kubectl apply -f .
```

You can reapply at will; Dhall ensures stable output unless you change inputs.

Check resource health:

```
kubectl get all -n <namespace>
kubectl describe <resource> -n <namespace>
kubectl logs <pod> -n <namespace>
```

If you are working with multiple namespaces, ensure your manifests explicitly set them. Nothing here assumes defaults.

## Secret Handling

Secrets in this directory are defined structurally in Dhall but must contain already-encoded values. Dhall does not compute base64 or generate Docker registry auth blobs. CI or a local script should prepare those values and export them as environment variables before rendering.

A typical pattern:

```
export OCI_REGISTRY_AUTH=$(cat dockerconfig.json | base64 -w0)
dhall-to-yaml --file ./oci-secret.dhall > oci-secret.yaml
```

This keeps Dhall deterministic while still allowing your cluster to receive fully-formed secrets.

## Typical Usage Pattern (Local Development)

1. Start Minikube.
2. Build your local images (if applicable).
3. Export any environment variables needed by Dhall.
4. Render all `.dhall` files into `.yaml`.
5. `kubectl apply -f .`
6. Iterate.

This directory should be treated as an authoritative declarative spec for local cluster state. Anything non-declarative belongs in CI or tooling outside Dhall.

If you want this expanded into a more complete structure (layout, conventions, or CI integration), I can flesh it out.
## Important Notes

* Config is setup to use a proxy, the file `proxy-ca-cert.dhall` needs updated with the base64 string for your certificate.
* GitLab Site certificate also needs updated for your host. `gitlab-crt.dhall` needs updated to the base64 string for your certificate.
* The token for access to your GitLab needs updated as well. The file `gitlab-secret.dhall` contains that secret.
* The `GITLAB_ENDPOINT` variable needs updated to point at your GitLab Server. It is located in the `gitlab-agent-deployment.dhall`.
