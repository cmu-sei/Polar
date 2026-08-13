# polar-nu-init

A minimal nushell init container for Polar infrastructure.

## Purpose

This container exists as a generic, reusable init container runner. Its only
job is to execute `/scripts/init.nu` — a nushell script mounted in by the
pod spec at runtime. The container has no opinion about what the script does.

Infrastructure automation (`infra/layers/`) owns the scripts. This container
owns the runtime.

## Contract

Mount a nushell script at `/scripts/init.nu`. The container will execute it
and exit. If the mount is missing, the container fails loudly with a clear
error message.

```yaml
initContainers:
  - name: nu-init
    image: polar-nu-init:latest
    volumeMounts:
      - name: init-script
        mountPath: /scripts
volumes:
  - name: init-script
    configMap:
      name: <your-configmap>
      items:
        - key: init.nu
          path: init.nu
```

## Why nushell?

Polar uses nushell throughout its infrastructure pipeline. Init container
scripts written in nushell are consistent with the rest of the toolchain,
have access to structured data operations, and avoid the escaping hazards
of bash for complex setup tasks.

## Building

```bash
# From repo root
nix build .#packages.<platform>.nuInitImage

# Or via just
just nu-init
```

## Rendering the container config

The `.nix` file is pre-rendered from `container.dhall` so Nix can import it
without network access in the sandbox:

```bash
cd src/containers/nu-init
just render
```

Commit both `container.dhall` and `container.nix` together.
