/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! What a Polar binary says it is when asked (`--version`, `--version --json`), and the `service.version` its
//! traces carry: the one place the platform version is embedded.
//!
//! It embeds only what changes at a release: the platform version (the repository's `VERSION` file, which build.rs
//! reads), the Cassini protocol version, the target the binary was built for and its allocator. Never the git commit:
//! that would change every binary, and so every image digest, at every commit, a documentation commit included. It
//! would defeat Nix's cache for the final crates and the release's check that its artifacts really changed, which
//! compares the lock's digests. The commit a release was built from is in the release's lock and changelog, outside
//! every binary and image (infra/INFRA.md, "The release").
//!
//! Between releases `VERSION` reads `<last release>+dev`: semver build metadata, so a development build never passes
//! for a release. A release sets it to `X.Y.Z` in a commit of its own and back to `X.Y.Z+dev` after the tag
//! (infra/release.nu).

use serde::{Deserialize, Serialize};
use std::fmt;

/// The platform version: the repository's `VERSION`, as it was when this crate was built.
pub const PLATFORM: &str = env!("POLAR_PLATFORM_VERSION");

/// The target triple the binary was built for.
pub const TARGET: &str = env!("POLAR_TARGET");

/// The global allocator. Every Polar binary allocates with Scudo, LLVM's hardened allocator, on every target it is
/// built for: the musl targets of the images and the tools, and the host's gnu target for tests. `polar-scudo` links
/// it as a static library (`libpolar_scudo.a`, built by the nix-scudo flake input, from `POLAR_SCUDO_LIB_DIR`) and
/// each binary declares `#[global_allocator] static GLOBAL: polar_scudo::Scudo`. This crate cannot see a binary's
/// allocator; `polar-scudo/tests/every_binary.rs` holds every binary of the workspace to that declaration, which is
/// what makes this constant true (docs/architecture/broker-baseline.md, "Decided").
pub const ALLOCATOR: &str = "scudo";

/// The Cassini protocol version the binary speaks.
///
/// HOOK, not yet filled: `cassini_types::PROTOCOL_VERSION` does not exist on main; another branch adds it. When it
/// lands, this crate depends on `cassini-types` and this becomes `Some(cassini_types::PROTOCOL_VERSION)` (formatted as
/// text if it is a number). Until then every record says `null`, rather than a number made up here.
pub const CASSINI_PROTOCOL: Option<&str> = None;

/// What `VERSION` ends with between releases.
pub const DEVELOPMENT_SUFFIX: &str = "+dev";

const _: () = assert!(
    well_formed(PLATFORM),
    "the repository's VERSION is not X.Y.Z (a release) or X.Y.Z+dev (between releases)"
);

/// A platform version: `X.Y.Z` for a release, `X.Y.Z+dev` between releases. Nothing else is one: no `v`, no
/// pre-release, no other build metadata, no leading zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// `+dev`: a build between releases.
    pub development: bool,
}

/// Why a text is not a platform version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionError(pub String);

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for VersionError {}

impl PlatformVersion {
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        let refuse = || {
            VersionError(format!(
                "{text:?} is not a platform version: X.Y.Z for a release, X.Y.Z+dev between releases"
            ))
        };
        let (core, development) = match text.split_once('+') {
            None => (text, false),
            Some((core, "dev")) => (core, true),
            Some(_) => return Err(refuse()),
        };
        let number = |part: &str| -> Option<u64> {
            let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            if !digits || (part.len() > 1 && part.starts_with('0')) {
                return None;
            }
            part.parse().ok()
        };
        let parts: Vec<&str> = core.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            return Err(refuse());
        };
        Ok(Self {
            major: number(major).ok_or_else(refuse)?,
            minor: number(minor).ok_or_else(refuse)?,
            patch: number(patch).ok_or_else(refuse)?,
            development,
        })
    }

    pub fn is_release(&self) -> bool {
        !self.development
    }

    /// The release of the same numbers.
    pub fn as_release(self) -> Self {
        Self {
            development: false,
            ..self
        }
    }

    /// The development version after the release of the same numbers: what `VERSION` reads once it is tagged.
    pub fn as_development(self) -> Self {
        Self {
            development: true,
            ..self
        }
    }
}

impl fmt::Display for PlatformVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if self.development {
            f.write_str(DEVELOPMENT_SUFFIX)?;
        }
        Ok(())
    }
}

/// Whether `text` is a platform version, at compile time: the check that stops a build whose `VERSION` is not one.
/// The same rule as [`PlatformVersion::parse`], written without allocating so it can run in a constant.
pub const fn well_formed(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    let mut part = 0;
    while part < 3 {
        let start = i;
        let mut value: u64 = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            value = match value.checked_mul(10) {
                Some(v) => match v.checked_add((b[i] - b'0') as u64) {
                    Some(v) => v,
                    None => return false,
                },
                None => return false,
            };
            i += 1;
        }
        if i == start || (i - start > 1 && b[start] == b'0') {
            return false;
        }
        part += 1;
        if part < 3 {
            if i < b.len() && b[i] == b'.' {
                i += 1;
            } else {
                return false;
            }
        }
    }
    let rest = b.len() - i;
    rest == 0
        || (rest == 4 && b[i] == b'+' && b[i + 1] == b'd' && b[i + 2] == b'e' && b[i + 3] == b'v')
}

/// What a binary says it is: `--version --json` prints it, `--version` its [`line`](VersionRecord::line), and an
/// agent's scaffold carries it as `agent.version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRecord {
    /// The agent's name, or the binary's for one that is not an agent.
    pub name: String,
    /// The platform version.
    pub version: String,
    /// Whether the version is a release's (`X.Y.Z`) and not a development build's (`X.Y.Z+dev`).
    pub release: bool,
    /// The Cassini protocol version; null until it is defined (see [`CASSINI_PROTOCOL`]).
    pub cassini_protocol: Option<String>,
    /// The target triple.
    pub target: String,
    /// The global allocator.
    pub allocator: String,
}

/// The record of the binary called `name`.
pub fn record(name: &str) -> VersionRecord {
    VersionRecord {
        name: name.to_string(),
        version: PLATFORM.to_string(),
        release: PlatformVersion::parse(PLATFORM).is_ok_and(|v| v.is_release()),
        cassini_protocol: CASSINI_PROTOCOL.map(str::to_string),
        target: TARGET.to_string(),
        allocator: ALLOCATOR.to_string(),
    }
}

impl VersionRecord {
    /// One line, for a person: `cassini-server 1.0.0+dev (development build, x86_64-unknown-linux-musl, allocator
    /// scudo)`.
    pub fn line(&self) -> String {
        let kind = if self.release {
            "release"
        } else {
            "development build"
        };
        let protocol = match &self.cassini_protocol {
            Some(p) => format!(", cassini protocol {p}"),
            None => String::new(),
        };
        format!(
            "{} {} ({kind}{protocol}, {}, allocator {})",
            self.name, self.version, self.target, self.allocator
        )
    }

    /// The record as one line of JSON.
    pub fn json(&self) -> String {
        serde_json::to_string(self).expect("a record of strings and a flag is JSON")
    }
}

/// What a binary called `name` prints for `args` (the arguments after the program's name) when they ask for its
/// version: `--version` gives the line, `--version --json` the record. None for any other arguments, which are the
/// binary's own. For a binary that is not built on the agent builder, which answers `--version` itself.
pub fn answer(name: &str, args: &[String]) -> Option<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version"] => Some(record(name).line()),
        ["--version", "--json"] => Some(record(name).json()),
        _ => None,
    }
}

/// Prints the answer and exits 0 when the process's arguments ask for its version; returns otherwise. The first line
/// of a `main` that parses its own arguments.
pub fn exit_if_asked(name: &str) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(text) = answer(name, &args) {
        println!("{text}");
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_release_and_a_development_version_are_told_apart() {
        let release = PlatformVersion::parse("1.2.3").unwrap();
        assert_eq!(
            release,
            PlatformVersion {
                major: 1,
                minor: 2,
                patch: 3,
                development: false
            }
        );
        assert!(release.is_release());
        let dev = PlatformVersion::parse("1.2.3+dev").unwrap();
        assert!(!dev.is_release());
        assert_eq!(dev.as_release(), release);
        assert_eq!(release.as_development(), dev);
        assert_eq!(release.to_string(), "1.2.3");
        assert_eq!(dev.to_string(), "1.2.3+dev");
        assert_eq!(
            PlatformVersion::parse("0.0.0").unwrap().to_string(),
            "0.0.0"
        );
    }

    #[test]
    fn anything_else_is_not_a_platform_version() {
        for text in [
            "",
            "1",
            "1.0",
            "1.0.0.0",
            "v1.0.0",
            " 1.0.0",
            "1.0.0 ",
            "1.0.0\n",
            "01.0.0",
            "1.00.0",
            "1.0.01",
            "1.0.0+",
            "1.0.0+dev2",
            "1.0.0+DEV",
            "1.0.0+build.5",
            "1.0.0-rc.1",
            "1.0.0-rc.1+dev",
            "1..0",
            "1.0.",
            ".1.0",
            "1.a.0",
            "+dev",
            "18446744073709551616.0.0",
        ] {
            assert!(PlatformVersion::parse(text).is_err(), "{text:?} accepted");
            assert!(!well_formed(text), "{text:?} well formed");
        }
        assert!(well_formed("18446744073709551615.0.0+dev"));
    }

    #[test]
    fn the_embedded_version_is_a_platform_version() {
        assert!(PlatformVersion::parse(PLATFORM).is_ok(), "{PLATFORM}");
    }

    #[test]
    fn the_record_says_what_the_binary_is_and_nothing_of_its_commit() {
        let r = record("jira-observer");
        assert_eq!(r.name, "jira-observer");
        assert_eq!(r.version, PLATFORM);
        assert_eq!(
            r.release,
            PlatformVersion::parse(PLATFORM).unwrap().is_release()
        );
        assert_eq!(r.cassini_protocol, None);
        assert_eq!(r.target, TARGET);
        assert_eq!(r.allocator, "scudo");

        let json: serde_json::Value = serde_json::from_str(&r.json()).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "allocator",
                "cassini_protocol",
                "name",
                "release",
                "target",
                "version"
            ]
        );
        assert_eq!(json["cassini_protocol"], serde_json::Value::Null);
        assert_eq!(json["release"], serde_json::Value::Bool(r.release));
        let back: VersionRecord = serde_json::from_str(&r.json()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn the_line_says_whether_it_is_a_release() {
        let mut r = VersionRecord {
            name: "cassini-server".into(),
            version: "1.2.0".into(),
            release: true,
            cassini_protocol: None,
            target: "x86_64-unknown-linux-musl".into(),
            allocator: "scudo".into(),
        };
        assert_eq!(
            r.line(),
            "cassini-server 1.2.0 (release, x86_64-unknown-linux-musl, allocator scudo)"
        );
        r.version = "1.2.0+dev".into();
        r.release = false;
        r.cassini_protocol = Some("3".into());
        assert_eq!(
            r.line(),
            "cassini-server 1.2.0+dev (development build, cassini protocol 3, x86_64-unknown-linux-musl, allocator scudo)"
        );
    }

    #[test]
    fn only_the_two_version_requests_are_answered() {
        let line = answer("polar-sign", &strings(&["--version"])).unwrap();
        assert_eq!(line, record("polar-sign").line());
        let json = answer("polar-sign", &strings(&["--version", "--json"])).unwrap();
        assert_eq!(
            serde_json::from_str::<VersionRecord>(&json).unwrap(),
            record("polar-sign")
        );
        for other in [
            &[][..],
            &["--json"][..],
            &["--json", "--version"][..],
            &["--version", "--yaml"][..],
            &["--version", "--json", "x"][..],
            &["sign", "--version"][..],
            &["-V"][..],
        ] {
            assert_eq!(answer("polar-sign", &strings(other)), None, "{other:?}");
        }
    }
}
