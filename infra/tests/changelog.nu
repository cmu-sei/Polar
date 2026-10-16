#!/usr/bin/env nu

# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# CHANGELOG.md: how a release's section is written, and how the file is kept.

use std/assert
use ../changelog.nu [file_header, image_changes, md_banner, render_section, split_file, with_section]

const HERE = (path self | path dirname)

def e [sha: string, type: string, scope: string, description: string, significance: string, kind: string = "conventional", breaking: bool = false, note: string = ""] {
    { sha: $sha, subject: $description, kind: $kind, type: $type, scope: $scope, description: $description, breaking: $breaking, breaking_note: $note, significance: $significance, note: "" }
}

def config [] { open ($HERE | path dirname | path join "changelog.nuon") }

def release [version: string = "1.3.0"] { { version: $version, date: "2026-11-02", previous: "1.2.0", highlights: "" } }

def section [entries: list, images: list = [], rel: record = {}] {
    render_section (release | merge $rel) $entries $images (config)
}

def lock [image: string, tag: string, arch: string, digest: string] { { image: $image, tag: $tag, arch: $arch, digest: $digest } }

# (A raw string that starts with ## does not parse in nushell, so this one starts with a newline that the test removes.)
const GOLDEN_RAW = r#'
## [1.3.0] - 2026-11-02

Full diff: https://example.org/polar/compare/v1.2.0...v1.3.0

> Two lines of words.
> The second line.

### 💥 Breaking changes

- *(credentials-agent)* Signed admin actions
  the old CLI is refused, rebuild it

### 📦 Images

| Image | Change |
|---|---|
| `cassini:latest` | `sha256:aaaaaaaaaaaa` -> `sha256:bbbbbbbbbbbb` |
| `newone:v1` | new, `sha256:cccccccccccc` |

### 🚀 Features

**authority**

- The decision has three values

**cassini**

- Add a retry to the client

**Other**

- Add something people asked for †

### 🐛 Bug fixes

**cassini**

- Stop a send on a closed socket

### ⚡ Performance

**cassini**

- Avoid a copy

† Classified from its wording: the commit was not written as a conventional commit.

Also in this release, not listed: 2 documentation and 1 housekeeping commits.

<details><summary>1 change was not written as a conventional commit and is counted as a patch</summary>

- typo (`6666666`)

</details>'#

def main [] {
    let tests = [
        {
            name: "the banner is the one the other Markdown files start with"
            body: {
                let readme = (open --raw ($HERE | path dirname | path dirname | path join "README.md") | lines)
                let end = ($readme | enumerate | where {|r| $r.item == "-->" } | first | get index)
                assert equal (md_banner) ($readme | first ($end + 1) | str join "\n")
            }
        }
        {
            name: "a section with every kind of entry is written exactly as it should be"
            body: {
                let entries = [
                    (e "1111111aaa" "feat" "cassini" "add a retry to the client." "feature")
                    (e "2222222bbb" "feat" "authority" "the decision has three values" "feature")
                    (e "3333333ccc" "fix" "cassini" "stop a send on a closed socket" "fix")
                    (e "4444444ddd" "feat" "credentials-agent" "signed admin actions" "breaking" "conventional" true "the old CLI is refused, rebuild it")
                    (e "5555555eee" "feat" "" "Add something people asked for" "feature" "inferred")
                    (e "6666666fff" "fix" "" "typo" "fix" "unclassified")
                    (e "7777777ggg" "docs" "" "explain" "none")
                    (e "8888888hhh" "docs" "" "explain more" "none")
                    (e "9999999iii" "chore" "" "tidy" "none")
                    (e "aaaaaaajjj" "feat" "x" "added then reverted" "none" "cancelled")
                    (e "bbbbbbbkkk" "perf" "cassini" "avoid a copy" "fix")
                ]
                let images = [
                    { image: "cassini", tag: "latest", before: "sha256:aaaaaaaaaaaa1111", after: "sha256:bbbbbbbbbbbb2222", change: "changed" }
                    { image: "newone", tag: "v1", before: "", after: "sha256:cccccccccccc3333", change: "new" }
                ]
                let text = (render_section { version: "1.3.0", date: "2026-11-02", previous: "1.2.0", highlights: "Two lines of words.\nThe second line." } $entries $images ((config) | update repository "https://example.org/polar"))
                assert equal $text ($GOLDEN_RAW | str replace --regex '^\n' '')
            }
        }
        {
            name: "an empty group is left out, and nothing at all is left out when there is nothing to say but a version"
            body: {
                let text = (section [(e "1111111aaa" "feat" "a" "one thing" "feature")])
                assert ($text | str contains "### 🚀 Features")
                for gone in ["Breaking changes" "Images" "Bug fixes" "Performance" "Also in this release" "conventional commit"] {
                    assert (not ($text | str contains $gone)) $"($gone) should not appear"
                }
                let bare = (render_section { version: "1.3.0", date: "2026-11-02", previous: "", highlights: "" } [] [] (config))
                assert equal $bare "## [1.3.0] - 2026-11-02"
            }
        }
        {
            name: "the sections come in the order the config gives them"
            body: {
                let entries = [(e "1111111aaa" "fix" "a" "mend" "fix") (e "2222222bbb" "feat" "a" "add" "feature") (e "3333333ccc" "feat" "a" "break" "breaking" "conventional" true)]
                let images = [{ image: "a", tag: "1", before: "", after: "sha256:dddddddddddd", change: "new" }]
                let text = (section $entries $images)
                let at = {|h| $text | str index-of $h }
                assert ((do $at "### 💥") < (do $at "### 📦"))
                assert ((do $at "### 📦") < (do $at "### 🚀"))
                assert ((do $at "### 🚀") < (do $at "### 🐛"))
            }
        }
        {
            name: "scopes are in order, the unscoped entries are last, and a scope that names several is grouped under the first"
            body: {
                let entries = [
                    (e "1111111aaa" "feat" "zeta" "z thing" "feature")
                    (e "2222222bbb" "feat" "" "no scope" "feature")
                    (e "3333333ccc" "feat" "alpha,zeta" "shared" "feature")
                ]
                let text = (section $entries)
                assert ((($text | str index-of "**alpha**") < ($text | str index-of "**zeta**")) and (($text | str index-of "**zeta**") < ($text | str index-of "**Other**")))
                assert ($text | str contains "- Shared (also: zeta)")
            }
        }
        {
            name: "a description is capitalised and loses its final full stop, and nothing else of it changes"
            body: {
                let text = (section [(e "1111111aaa" "feat" "a" "add the thing, then the other." "feature")])
                assert ($text | lines | any {|l| $l == "- Add the thing, then the other"}) "capitalised, and the final full stop gone"
            }
        }
        {
            name: "a long group shows a count for each scope and the entries in a collapsed list"
            body: {
                let many = (0..49 | each {|i| e $"($i)000000aaa" "feat" (if $i < 30 { "big" } else { "small" }) $"entry ($i)" "feature" })
                let text = (section $many)
                assert ($text | str contains "- **big**: 30")
                assert ($text | str contains "- **small**: 20")
                assert ($text | str contains "<details><summary>All 50 entries</summary>")
                let few = (section ($many | first 5))
                assert (not ($few | str contains "<details>")) "a short group is listed plainly"
            }
        }
        {
            name: "an entry whose type was inferred has a dagger and the footnote, and without one there is neither"
            body: {
                let with = (section [(e "1111111aaa" "feat" "" "Add a thing" "feature" "inferred")])
                assert ($with | str contains "- Add a thing †")
                assert ($with | str contains "† Classified from its wording")
                let without = (section [(e "1111111aaa" "feat" "a" "add a thing" "feature")])
                assert (not ($without | str contains "†"))
            }
        }
        {
            name: "what is not listed is counted in a sentence, a cancelled pair is not, and nothing counted is no sentence"
            body: {
                let one = (section [(e "1111111aaa" "docs" "" "x" "none")])
                assert ($one | str contains "not listed: 1 documentation commit.")
                let three = (section [(e "1" "docs" "" "x" "none") (e "2" "docs" "" "x" "none") (e "3" "chore" "" "x" "none") (e "4" "test" "" "x" "none") (e "5" "feat" "" "x" "none" "cancelled")])
                assert ($three | str contains "not listed: 2 documentation, 1 housekeeping and 1 test commits.")
                assert (not ((section [(e "1111111aaa" "feat" "a" "x" "feature")]) | str contains "not listed"))
            }
        }
        {
            name: "a breaking change says what to do, from its footer, and without one it is just the line"
            body: {
                let text = (section [(e "1111111aaa" "feat" "agent" "change it" "breaking" "conventional" true "rebuild the CLI") (e "2222222bbb" "fix" "" "other" "breaking" "conventional" true)])
                assert ($text | str contains "- *(agent)* Change it\n  rebuild the CLI")
                assert ($text | lines | any {|l| $l == "- Other"}) "no scope, no footer"
            }
        }
        {
            name: "changes that could not be classified are in a collapsed list, with the right grammar for one and for many"
            body: {
                let one = (section [(e "abcdef1234" "fix" "" "typo" "fix" "unclassified")])
                assert ($one | str contains "1 change was not written as a conventional commit and is counted as a patch")
                assert ($one | str contains "- typo (`abcdef1`)")
                let two = (section [(e "abcdef1234" "fix" "" "typo" "fix" "unclassified") (e "bcdef12345" "fix" "" "oops" "fix" "unclassified")])
                assert ($two | str contains "2 changes were not written as conventional commits and are counted as patches")
            }
        }
        {
            name: "the highlights are quoted line by line, and the link to the full diff appears only when the repository is known"
            body: {
                let q = (section [] [] { highlights: "First line.\nSecond line." })
                assert ($q | str contains "> First line.\n> Second line.")
                assert (not ($q | str contains "Full diff")) "the shipped config names no repository"
                let first = (render_section { version: "0.2.0", date: "2026-11-02", previous: "", highlights: "" } [] [] ((config) | update repository "https://example.org/p"))
                assert (not ($first | str contains "Full diff")) "a first release has nothing to compare with"
            }
        }
        {
            name: "image changes are the new, the changed and the removed ones, for the architecture asked for, and not the unchanged"
            body: {
                let before = [(lock "same" "1" "amd64" "sha256:aa") (lock "moved" "1" "amd64" "sha256:bb") (lock "gone" "1" "amd64" "sha256:cc") (lock "moved" "1" "arm64" "sha256:zz")]
                let after = [(lock "same" "1" "amd64" "sha256:aa") (lock "moved" "1" "amd64" "sha256:b2") (lock "fresh" "1" "amd64" "sha256:dd") (lock "moved" "1" "arm64" "sha256:z2")]
                let c = (image_changes $before $after "amd64")
                assert equal ($c | get image) ["fresh" "gone" "moved"]
                assert equal ($c | where image == "fresh" | first | get change) "new"
                assert equal ($c | where image == "gone" | first | get change) "removed"
                let m = ($c | where image == "moved" | first)
                assert equal [$m.change $m.before $m.after] ["changed" "sha256:bb" "sha256:b2"]
                assert equal (image_changes $before $before "amd64") []
            }
        }
        {
            name: "a new file has the banner, the title and the intro, then the section"
            body: {
                let text = (with_section "" "0.2.0" "## [0.2.0] - 2026-11-02\n\nbody" (config))
                assert ($text | str starts-with (md_banner))
                assert ($text | str contains "# Changelog\n\nAll notable changes")
                assert ($text | str ends-with "## [0.2.0] - 2026-11-02\n\nbody\n")
            }
        }
        {
            name: "a newer release goes on top, and writing a version again replaces only that section"
            body: {
                let one = (with_section "" "0.2.0" "## [0.2.0] - 2026-11-02\n\nfirst" (config))
                let two = (with_section $one "0.3.0" "## [0.3.0] - 2026-12-01\n\nsecond" (config))
                assert ((($two | str index-of "## [0.3.0]") < ($two | str index-of "## [0.2.0]")))
                let again = (with_section $two "0.3.0" "## [0.3.0] - 2026-12-01\n\nsecond, regenerated" (config))
                assert ($again | str contains "second, regenerated")
                assert (not ($again | str contains "\nsecond\n"))
                assert equal ((split_file $again).sections | where version == "0.2.0" | first | get text) ((split_file $two).sections | where version == "0.2.0" | first | get text) "the older section is untouched"
                assert equal (with_section $again "0.3.0" "## [0.3.0] - 2026-12-01\n\nsecond, regenerated" (config)) $again "writing the same thing twice changes nothing"
            }
        }
        {
            name: "a hand-edited older section survives a new release byte for byte"
            body: {
                let hand = $"(file_header (config))\n\n## [0.2.0] - 2026-11-02\n\nA paragraph a person wrote, with `code` and a [link]\(x\).\n\n- an entry\n"
                let after = (with_section $hand "0.3.0" "## [0.3.0] - 2026-12-01\n\nnew" (config))
                assert ($after | str contains "A paragraph a person wrote, with `code` and a [link](x).\n\n- an entry")
            }
        }
    ]

    let results = ($tests | each {|t|
        try { do $t.body; { name: $t.name, error: null } } catch {|e| { name: $t.name, error: $e.msg } }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 { print -e $"($failed) of ($results | length) failed"; exit 1 }
    print $"all ($results | length) passed"
}
