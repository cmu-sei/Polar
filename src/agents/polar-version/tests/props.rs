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

// The rules of a platform version, for every input: what is written is read back, the compile-time check and the
// parser agree on every text, and a release and a development version are each other's with the numbers kept.

use polar_version::{PlatformVersion, VersionRecord, answer, well_formed};
use proptest::prelude::*;

fn version() -> impl Strategy<Value = PlatformVersion> {
    (any::<u64>(), 0..1000u64, 0..1000u64, any::<bool>()).prop_map(
        |(major, minor, patch, development)| PlatformVersion {
            major: major % 100_000,
            minor,
            patch,
            development,
        },
    )
}

/// Texts near the grammar, from pieces of right and wrong versions, so that most of them are close calls.
fn near_version() -> impl Strategy<Value = String> {
    let piece = prop::sample::select(vec![
        "0",
        "1",
        "7",
        "01",
        "12",
        "18446744073709551615",
        "18446744073709551616",
        ".",
        "..",
        "+dev",
        "+",
        "dev",
        "-rc.1",
        "v",
        " ",
        "\n",
        "+dev2",
        "a",
    ]);
    prop::collection::vec(piece, 0..8).prop_map(|p| p.concat())
}

proptest! {
    #[test]
    fn a_version_written_is_read_back(v in version()) {
        let text = v.to_string();
        prop_assert_eq!(PlatformVersion::parse(&text), Ok(v));
        prop_assert!(well_formed(&text));
    }

    #[test]
    fn the_compile_time_check_and_the_parser_agree(text in prop_oneof![near_version(), "\\PC{0,14}"]) {
        prop_assert_eq!(well_formed(&text), PlatformVersion::parse(&text).is_ok(), "{:?}", text);
    }

    #[test]
    fn a_release_never_carries_build_metadata_and_its_development_version_keeps_its_numbers(v in version()) {
        let release = v.as_release();
        prop_assert!(release.is_release());
        prop_assert!(!release.to_string().contains('+'));
        let dev = v.as_development();
        prop_assert!(!dev.is_release());
        prop_assert_eq!(dev.to_string(), format!("{release}+dev"));
        prop_assert_eq!(dev.as_release(), release);
        prop_assert_eq!((dev.major, dev.minor, dev.patch), (v.major, v.minor, v.patch));
    }

    #[test]
    fn arguments_other_than_the_two_requests_are_left_to_the_binary(
        args in prop::collection::vec(
            prop::sample::select(vec!["--version", "--json", "--help", "-V", "sign", "--config"]).prop_map(String::from),
            0..4,
        )
    ) {
        let asked = args == ["--version"] || args == ["--version", "--json"];
        prop_assert_eq!(answer("tool", &args).is_some(), asked, "{:?}", args);
        if asked && args.len() == 2 {
            let r: VersionRecord = serde_json::from_str(&answer("tool", &args).unwrap()).unwrap();
            prop_assert_eq!(r.name, "tool");
        }
    }
}
