<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

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
