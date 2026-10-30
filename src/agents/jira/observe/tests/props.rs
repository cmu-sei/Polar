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

//! Properties of the Jira observer's configuration checks.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use jira_observer::settings::{normalise_jira_url, token_looks_base64};
use proptest::prelude::*;
use url::Url;

/// Every path the observers append to the address.
const ENDPOINTS: &[&str] = &[
    "/rest/api/2/myself",
    "/rest/api/2/project",
    "/rest/api/2/groups/picker",
    "/rest/api/2/user/picker",
    "/rest/api/2/field",
    "/rest/api/2/search",
    "/rest/api/3/search/jql",
];

/// A path segment that is not `.` or `..`, which the URL parser would fold away.
fn segment() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_~-][a-zA-Z0-9._~-]{0,7}"
}

/// An address someone might set: http or https, a host, perhaps a port, perhaps a context path,
/// perhaps trailing slashes.
fn site() -> impl Strategy<Value = String> {
    (
        prop_oneof![Just("http"), Just("https")],
        "[a-zA-Z][a-zA-Z0-9-]{0,8}(\\.[a-zA-Z][a-zA-Z0-9]{0,6}){0,2}",
        proptest::option::of(1u16..),
        proptest::collection::vec(segment(), 0..4),
        0usize..4,
    )
        .prop_map(|(scheme, host, port, segments, slashes)| {
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            let path: String = segments.iter().map(|s| format!("/{s}")).collect();
            format!("{scheme}://{host}{port}{path}{}", "/".repeat(slashes))
        })
}

/// Anything at all, or an address someone might set.
fn anything() -> impl Strategy<Value = String> {
    prop_oneof![site(), any::<String>(), "[a-z:/.]{0,24}"]
}

proptest! {
    #[test]
    fn normalising_twice_is_normalising_once(given in anything()) {
        if let Ok(once) = normalise_jira_url(&given) {
            prop_assert_eq!(normalise_jira_url(&once), Ok(once));
        }
    }

    #[test]
    fn no_endpoint_built_from_a_normalised_address_has_a_doubled_slash_before_rest(
        given in anything()
    ) {
        if let Ok(address) = normalise_jira_url(&given) {
            for op in ENDPOINTS {
                let endpoint = format!("{address}{op}");
                // The scheme's own "//" is not the path's: a host may be called rest.example.org.
                let after_scheme = &endpoint[endpoint.find("://").unwrap() + 3..];
                prop_assert!(!after_scheme.contains("//rest"), "{}", endpoint);
                let parsed = Url::parse(&endpoint).unwrap();
                prop_assert!(parsed.path().ends_with(op), "{}", endpoint);
            }
        }
    }

    #[test]
    fn an_address_that_passes_keeps_its_scheme_and_host(given in anything()) {
        if let Ok(address) = normalise_jira_url(&given) {
            let before = Url::parse(given.trim()).unwrap();
            let after = Url::parse(&address).unwrap();
            prop_assert_eq!(before.scheme(), after.scheme());
            prop_assert_eq!(before.host_str(), after.host_str());
            prop_assert_eq!(before.port(), after.port());
        }
    }

    #[test]
    fn a_site_with_no_rest_segment_passes(given in site()) {
        let url = Url::parse(&given).unwrap();
        let has_rest = url
            .path_segments()
            .is_some_and(|mut s| s.any(|s| s.eq_ignore_ascii_case("rest")));
        prop_assume!(!has_rest && url.host_str() != Some("example.atlassian.net"));
        prop_assert!(normalise_jira_url(&given).is_ok(), "{}", given);
    }

    #[test]
    fn an_address_with_a_rest_segment_is_refused(
        given in site(),
        rest in prop_oneof![Just("rest"), Just("REST"), Just("Rest")],
        after in proptest::collection::vec(segment(), 0..3),
        slashes in 0usize..3,
    ) {
        let tail: String = after.iter().map(|s| format!("/{s}")).collect();
        let given = format!(
            "{}/{rest}{tail}{}",
            given.trim_end_matches('/'),
            "/".repeat(slashes)
        );
        prop_assert!(normalise_jira_url(&given).is_err(), "{}", given);
    }

    #[test]
    fn a_scheme_other_than_http_or_https_is_refused(
        scheme in "[a-z][a-z0-9+.-]{0,6}",
        rest in "[a-z]{1,8}(\\.[a-z]{1,4})?(/[a-z]{1,4})?",
    ) {
        prop_assume!(scheme != "http" && scheme != "https");
        let given = format!("{scheme}://{rest}");
        prop_assert!(normalise_jira_url(&given).is_err(), "{}", given);
    }

    #[test]
    fn a_base64_copy_of_any_printable_token_is_noticed(token in "[!-~]{16,64}") {
        for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
            let encoded = engine.encode(&token);
            prop_assert!(token_looks_base64(&encoded), "{}", encoded);
        }
    }

    #[test]
    fn printable_text_with_one_other_byte_in_it_is_never_noticed(
        mut bytes in proptest::collection::vec(0x21u8..=0x7e, 15..63),
        odd in prop_oneof![0u8..=0x20, 0x7fu8..=0xff],
        at in any::<proptest::sample::Index>(),
    ) {
        let at = at.index(bytes.len() + 1);
        bytes.insert(at, odd);
        for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
            prop_assert!(!token_looks_base64(&engine.encode(&bytes)));
        }
    }

    #[test]
    fn a_data_center_shaped_token_is_not_noticed(blob in proptest::collection::vec(any::<u8>(), 33)) {
        // 33 random bytes as 44 characters of base64, the shape of a personal access token. All
        // 33 are printable with probability (94/256)^33, about 5e-15.
        prop_assert!(!token_looks_base64(&STANDARD.encode(&blob)));
    }
}
