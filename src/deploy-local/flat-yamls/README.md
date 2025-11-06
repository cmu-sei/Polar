# Kubernetes Yaml Files

## Important Notes

* Config is setup to use a proxy, the file `proxy-ca-cert.yaml` needs updated with the base64 string for your certificate.
* GitLab Site certificate also needs updated for your host. `gitlab-crt.yaml` needs updated to the base64 string for your certificate.
* The token for access to your GitLab needs updated as well. The file `gitlab-secret.yaml` contains that secret.
* The `GITLAB_ENDPOINT` variable needs updated to point at your GitLab Server. It is located in the `gitlab-agent-deployment.yaml`.

## Applying

You can use the `apply_all.sh` if you want to apply or do it manually on each for your local cluster.
