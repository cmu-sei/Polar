# Dhall

## Important Notes

* Config is setup to use a proxy, the file `proxy-ca-cert.dhall` needs updated with the base64 string for your certificate.
* GitLab Site certificate also needs updated for your host. `gitlab-crt.dhall` needs updated to the base64 string for your certificate.
* The token for access to your GitLab needs updated as well. The file `gitlab-secret.dhall` contains that secret.
* The `GITLAB_ENDPOINT` variable needs updated to point at your GitLab Server. It is located in the `gitlab-agent-deployment.dhall`.


