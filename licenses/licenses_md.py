#!/usr/bin/env python3

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

"""Render a deduped third-party license file from `cargo about generate --format json`.

Drops your own workspace crates (--exclude), groups by SPDX license id, and links
every crate to its repository (or crates.io when no repository is set).

  --mode compact  one section per license id: crate list + one canonical text (default)
  --mode full     one section per distinct (id, text), keeping every copyright notice
"""
import argparse
import json
import sys
from collections import Counter, defaultdict


def crate_url(c):
    return c.get("repository") or "https://crates.io/crates/%s" % c["name"]


def crate_line(name, version, url):
    return "- [%s %s](%s)" % (name, version, url)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("about_json")
    ap.add_argument("--exclude", help="file with one crate name per line (your own crates)")
    ap.add_argument("--mode", choices=["compact", "full"], default="compact")
    args = ap.parse_args()

    with open(args.about_json) as f:
        data = json.load(f)

    exclude = set()
    if args.exclude:
        with open(args.exclude) as f:
            exclude = {line.strip() for line in f if line.strip()}

    # key -> {"id", "name", "crates": {(name, version): url}, "texts": Counter}
    groups = {}
    skipped = set()
    for lic in data.get("licenses", []):
        for use in lic.get("used_by", []):
            c = use["crate"]
            if c["name"] in exclude:
                skipped.add(c["name"])
                continue
            key = lic["id"] if args.mode == "compact" else (lic["id"], lic["text"])
            g = groups.setdefault(
                key, {"id": lic["id"], "name": lic["name"], "crates": {}, "texts": Counter()}
            )
            g["crates"][(c["name"], c["version"])] = crate_url(c)
            g["texts"][lic["text"]] += 1

    ordered = sorted(groups.values(), key=lambda g: (-len(g["crates"]), g["id"]))

    # Summary is always per license id, with distinct crate counts.
    per_id = defaultdict(set)
    names = {}
    for g in ordered:
        per_id[g["id"]].update(g["crates"])
        names[g["id"]] = g["name"]

    out = ["# Third-Party Licenses", "", "## Summary", ""]
    for lid, crates in sorted(per_id.items(), key=lambda kv: (-len(kv[1]), kv[0])):
        out.append("- [%s](#%s) (%d crates)" % (names[lid], lid, len(crates)))
    out.append("")

    seen_anchor = set()
    for g in ordered:
        anchor = '<a id="%s"></a>' % g["id"] if g["id"] not in seen_anchor else ""
        seen_anchor.add(g["id"])
        out += ["## %s%s" % (anchor, g["name"]), "", "**Used by:**", ""]
        for (name, version), url in sorted(g["crates"].items()):
            out.append(crate_line(name, version, url))
        text = g["texts"].most_common(1)[0][0].strip("\n")
        out += ["", "````text", text, "````", ""]

    sys.stdout.write("\n".join(out))
    sys.stderr.write(
        "%d license ids, %d sections, excluded own crates seen: %s\n"
        % (len(per_id), len(ordered), ", ".join(sorted(skipped)) or "none")
    )


if __name__ == "__main__":
    main()
