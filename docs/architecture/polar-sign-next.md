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

# `polar-sign next`: the next version of a signed section, from the one in the repository

Status, 2026-11-05: **proposed**, for the maintainers to decide. Nothing is built.

## The problem

Changing one field of a signed runtime section means retyping the whole section. `polar-sign template` gives an empty file from the
agent's scaffold, not the section as it stands, so an administrator has to find their own copy of the last version, check it is the
version that was pushed, raise the version, change the field and sign it.

Seen on 2026-11-05, renaming the build-orchestrator's `pipeline_image` to `localhost/polar-ci@sha256:…` (one field, mappings version 3 to 4):

- The last version existed only as the administrator's own `mappings3.json`, in an admin directory named for the day the cluster was
  made (`~/.config/polar-admin-2026-11-04`). The default directory, `~/.config/polar-admin`, holds a previous cluster's certificate,
  which does not chain to the current CA.
- The tool's default clone, `~/.cache/polar-sign/schedules`, belonged to an earlier cluster. It held mappings version 10 and would
  have been the wrong place to start from. `push()` reuses a clone of the same remote as it is, so a clone left from a previous
  cluster also makes every push fail with "Not possible to fast-forward" (Blitz finding
  `2026-11-04-bringing-up-a-cluster-from-the-manuals.md`). The workaround is a new `--workdir` every time.

None of this is a defect of the signing. It is the work around it, and it is where a mistake (an old body, the wrong version) gets in.

## The command

```
polar-sign next <agent> [<section> | --settings]
    --repo <url> [--workdir <dir>] [--git-config KEY=VALUE]...
    [--ca <file>] [--dir <dir>] [--out <file>]
```

It writes the file `polar-sign sign` takes, filled in from what is in the schedules repository: `agent`, `version` N+1, `id`, and
`section` and `body` (or `settings`). The administrator edits what changes and signs it as now:

```sh
polar-sign next build-orchestrator mappings --repo "$SCHEDULES" --git-config "$RESOLVE" --out mappings.json
$EDITOR mappings.json
polar-sign sign mappings.json --user <user> --push --repo "$SCHEDULES" --git-config "$RESOLVE"
```

What it does, in order:

1. **Brings its clone up to date.** If the workdir is a clone of the same remote, `git fetch`, then reset to `origin`'s head. Otherwise
   clone. `push()` should do the same, which removes the "Not possible to fast-forward" failure for `sign --push` too.
2. **Reads the signed document** at `permanent/<agent>/<section>.dhall` (`push::place`). If there is none, it says so and points to
   `polar-sign template`.
3. **Verifies it as `polar-sign verify` does**: the layout, the chain to the CA (`--ca`, or `<dir>/ca.pem`), the signer's certificate
   and the signature. A document that does not verify produces no file. The tool never copies forward what it has not checked.
4. **Writes version N+1.** The body is the payload's, unchanged. Sealed secrets stay ciphertext: `seal.rs` already leaves a value that
   is ciphertext as it is, "so a file can be signed again", so nothing is decrypted or retyped. A comment header says where the file
   came from, which the lenient parser already accepts:
   `// from version 3, signed by vacoates (serial 4cc42d46…), schedules commit 1a2b3c4`.

**It signs nothing and reads no key.** Authority stays with `sign`, which still checks the file against the scaffold, seals and signs.
`next` only removes the retyping. It needs the administrator's certificate only for the git server's mTLS, as `sign --push` does.

## Size and tests

About 150 lines in a new `next.rs`, reusing `verify_source`, `push::place`, the `git` helper and `author::Input`. Tests:

- `read_input(next(document))` is the payload with the version raised by one, and nothing else changed;
- a document that does not verify (wrong CA, altered body) produces no file;
- a sealed value comes through byte for byte;
- a settings document as well as a section;
- a workdir whose clone is behind is brought up to date, not reused; a workdir that is another repository's clone is refused, as now.

## Open questions

1. **Does N+1 keep the document's `id`?** The file's `id` is optional, and `sign` makes a new one when it is absent. Keeping it says
   "the same document, its next version". Whether the receivers or the graph treat an `id` across versions in any particular way should
   be checked before choosing.
2. **The repository is not what the agent applied.** If the latest file was refused (a signer without the capability, a tag where a
   digest belongs), `next` builds on the refused version. N+1 is still higher, so nothing breaks, but the header should say "latest in
   the repository", not "applied". An agent's applied version is in its log only.
3. **`--set <pointer>=<value>` for scripted changes?** Proposed: not now. Opening the file in an editor makes the administrator look at
   what they sign.

## Not proposed

**Moving signing into the `polar` CLI.** That was decided on 2026-10-30: the CLI stands up plans, and protocol-heavy tools (`polar-sign`,
`provision-account`, `grant`, `revoke`) stay separate. The reasons still hold:

- `polar up` acts with cluster credentials, and `polar-sign` with a person's key. Agents accept or refuse what is signed whoever holds
  the cluster.
- A new version of a section is runtime policy, closer to `grant` than to standing up a plan.
- The protocol is already shared through `polar-signing`.
