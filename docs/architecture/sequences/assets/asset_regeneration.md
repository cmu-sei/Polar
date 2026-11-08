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

All sequence diagram sources live under `docs/architecture/sequences/`, organized by subsystem (`broker/`,
`general/`, `bootstrap/`). Each `.mmd` file has a matching `.svg` rendered with Mermaid CLI (`mmdc`). The diagrams
were PNGs until 2026-10-21; SVGs are a fifth of the size, scale without blurring, and their text can be searched.

Shared rendering assets (CSS, Mermaid config) live in `docs/architecture/sequences/assets/`.

## Regeneration

`mmdc` is available directly in Polar's dev shell (`direnv allow` from the repo root gets you a working environment
with no further setup).

```bash
nu docs/architecture/sequences/render.nu                       # every diagram
nu docs/architecture/sequences/render.nu general/foo.mmd       # the ones named, relative to sequences/
```

`render.nu` renders each diagram with `assets/mermaid-config.json` and a theme chosen by its kind: a
`sequenceDiagram` with `gruvbox_sequence.css`, anything else (a flowchart, a state diagram) with `gruvbox.css`. It
also:

- writes the diagram's license banner at the top of the SVG, as an XML comment, so the generated file carries it;
- sends each bare `%%` line of the banner to Mermaid as `%% `, because the flowchart parser runs bare ones together
  and fails (the source is left as it is);
- marks the SVG `xml:space="preserve"`: Mermaid puts each word of a flowchart label in a `tspan` of its own, with the
  space before it inside, and viewers other than browsers (librsvg) drop that space otherwise.

Flowchart labels are SVG text rather than HTML (`htmlLabels: false` in the config), so the diagrams show their
labels in any SVG viewer, not only in a browser.

## Notes

- Only edit `.mmd` files. The generated `.svg` files are versioned for convenience and reviewed as part of the diff
  when a diagram changes.
- `assets/mermaid-config.json` provides the Gruvbox base palette, transparent edge labels, and text labels.
- If `mmdc` fails with a Chromium `SingletonSocket` / socket-path-length error, it's almost always caused by running
  `mmdc` from inside a nested `nix-shell` (e.g. `nix-shell -p mermaid-cli.out --run "..."` invoked from within the
  already-active Polar dev shell) -- the resulting temp path exceeds the OS's Unix-socket path limit. This shouldn't
  come up in normal use, since `mmdc` is already on `PATH` in the dev shell directly; avoid wrapping it in a second
  `nix-shell` call.
