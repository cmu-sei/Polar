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

# Regenerating Diagram Assets

All sequence diagram sources live under `docs/architecture/sequences/`,
organized by subsystem (`broker/`, `general/`, `bootstrap/`). Each `.mmd` file has a
matching `.png` rendered with Mermaid CLI (`mmdc`).

Shared rendering assets (CSS, Mermaid config) live in
`docs/architecture/sequences/assets/`.

## Regeneration Process

`mmdc` is available directly in Polar's dev shell (`direnv allow` from the
repo root gets you a working environment with no further setup).

To regenerate every diagram:

```bash
cd docs/architecture/sequences
for f in broker/*.mmd general/*.mmd bootstrap/*.mmd; do
  name="${f%.mmd}"
  mmdc \
    -i "$f" \
    -o "$name.png" \
    --configFile assets/mermaid-config.json \
    --cssFile assets/gruvbox_sequence.css \
    --width 2560 \
    --height 1440
done
```

All of Polar's current sequence diagrams are interaction/sequence
diagrams, so `gruvbox_sequence.css` is used uniformly. If a
flow/deployment-style diagram (not a `sequenceDiagram`) is added later,
render it with `assets/gruvbox.css` instead -- see `metrics-rules-service`'s
own docs for the flow/sequence CSS distinction this was adapted from.

To regenerate a single diagram:

```bash
cd docs/architecture/sequences
mmdc \
  -i general/human_cert_issuance.mmd \
  -o general/human_cert_issuance.png \
  --configFile assets/mermaid-config.json \
  --cssFile assets/gruvbox_sequence.css \
  --width 2560 \
  --height 1440
```

## Notes

- Only edit `.mmd` files. Generated `.png` files are versioned for
  convenience and reviewed as part of the diff when a diagram changes.
- `assets/mermaid-config.json` provides the Gruvbox base palette and
  transparent edge labels.
- If `mmdc` fails with a Chromium `SingletonSocket` / socket-path-length
  error, it's almost always caused by running `mmdc` from inside a
  nested `nix-shell` (e.g. `nix-shell -p mermaid-cli.out --run "..."`
  invoked from within the already-active Polar dev shell) -- the
  resulting temp path exceeds the OS's Unix-socket path limit. This
  shouldn't come up in normal use, since `mmdc` is already on `PATH` in
  the dev shell directly; avoid wrapping it in a second `nix-shell`
  call.
