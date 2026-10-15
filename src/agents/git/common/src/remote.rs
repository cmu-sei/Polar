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

//! What the git agents agree on about a repository's URL and a credential's name, so that the controller that
//! accepts a repository and the observer that uses a credential for it cannot disagree.

/// Longest credential name.
pub const MAX_CREDENTIAL_NAME: usize = 64;

/// The host a credential is asked for: the host of an `https` URL with no user information in it. A
/// credential is never offered for any other scheme, so it cannot go over a connection that is not
/// encrypted, and a URL that carries its own login is refused rather than quietly overridden.
pub fn https_host(repo_url: &str) -> Result<String, String> {
    let url =
        url::Url::parse(repo_url).map_err(|_| "the repository URL cannot be read".to_string())?;
    if url.scheme() != "https" {
        return Err("a credential is only used for an https repository".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the repository URL carries a login of its own".to_string());
    }
    let host = match url.host() {
        Some(url::Host::Domain(domain)) => domain.to_string(),
        Some(_) => {
            return Err("a credential is only used for a host name, not an address".to_string());
        }
        None => return Err("the repository URL has no host".to_string()),
    };
    // libgit2 reads the URL for itself, and a credential must go where it was checked for. A URL the two
    // could read differently (a backslash, an `@`, an empty or odd authority) is refused: the text between
    // `https://` and the first `/`, `?` or `#` must be the host, and a port, and nothing else.
    let authority = repo_url
        .strip_prefix("https://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .unwrap_or_default();
    let (name, port) = match authority.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (authority, None),
    };
    let plain = port.is_none_or(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if !plain || !name.eq_ignore_ascii_case(&host) {
        return Err("the repository URL is not one a credential may be used for".to_string());
    }
    Ok(host)
}

/// Longest repository URL taken from anywhere.
pub const MAX_REPO_URL: usize = 2048;

/// A repository URL as the controller and the observer both read it: the one form a proposal's text is turned
/// into, so that nothing the observer is told depends on how libgit2 would have read the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalRepository {
    /// Scheme and host in lower case, no default port, no fragment, dot segments resolved, no trailing slash.
    pub url: String,
    /// The host name, in lower case.
    pub host: String,
    /// The path segments, as written: `["cmu-sei", "Polar.git"]`.
    pub path: Vec<String>,
}

impl CanonicalRepository {
    /// The id the graph and the observer know the repository by: the same for every spelling of it, and for
    /// `.git` or not.
    pub fn repo_id(&self) -> polar::graph::nodes::git::RepoId {
        use polar::graph::nodes::git::RepoId;
        RepoId::from_url(&RepoId::normalize_repo_url(&self.url))
    }
}

/// Reads untrusted text as the URL of a repository, or says why it is not one. Only `https`, only a host name
/// (never an address), no login, no query, no `%` in the path (a way to hide a `/`, a `\` or a dot segment),
/// no empty path segment, at least one segment, no space or control character, no trailing dot on the host.
/// What comes out is re-serialised, and that, not the original text, is what is ever passed on.
pub fn canonical_repository(raw: &str) -> Result<CanonicalRepository, String> {
    if raw.len() > MAX_REPO_URL {
        return Err(format!("the URL is longer than {MAX_REPO_URL} characters"));
    }
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the URL has a space or a control character".to_string());
    }
    if raw.contains('%') {
        return Err(
            "the URL has a % in it, which could hide a / or a dot segment from whoever checks it"
                .to_string(),
        );
    }
    // A scheme is case-insensitive; `https_host` compares the prefix exactly, so it is put in lower case first.
    let text = match raw.get(..8) {
        Some(prefix) if prefix.eq_ignore_ascii_case("https://") => format!("https://{}", &raw[8..]),
        _ => raw.to_string(),
    };
    // The same reading of the authority the observer uses for a credential: https, a host name, no login, and
    // nothing in the authority that two parsers could take differently.
    let host = https_host(&text)?;
    if host.ends_with('.') {
        return Err("the host has a trailing dot".to_string());
    }
    let mut url =
        url::Url::parse(&text).map_err(|_| "the repository URL cannot be read".to_string())?;
    if url.query().is_some() {
        return Err("the URL has a query, which a repository's address does not".to_string());
    }
    let mut segments: Vec<String> = url
        .path_segments()
        .map(|s| s.map(str::to_string).collect())
        .unwrap_or_default();
    if segments.last().is_some_and(String::is_empty) {
        segments.pop();
    }
    if segments.is_empty() {
        return Err("the URL names no repository".to_string());
    }
    if segments.iter().any(String::is_empty) {
        return Err("the URL has an empty path segment".to_string());
    }
    url.set_fragment(None);
    url.set_path(&format!("/{}", segments.join("/")));
    Ok(CanonicalRepository {
        url: url.to_string(),
        host,
        path: segments,
    })
}

/// Why a credential's name is not one, if it is not: lowercase letters, digits and hyphens, 1 to 64.
pub fn credential_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.len() > MAX_CREDENTIAL_NAME {
        Some("must be 1 to 64 characters")
    } else if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        Some("may have only lowercase letters, digits and hyphens")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_the_host_of_an_https_url_and_nothing_else_is_accepted() {
        assert_eq!(
            https_host("https://github.com/org/repo.git").unwrap(),
            "github.com"
        );
        assert_eq!(
            https_host("https://GitHub.com:8443/o/r.git").unwrap(),
            "github.com"
        );
        assert_eq!(https_host("https://github.com").unwrap(), "github.com");
        assert_eq!(https_host("https://github.com?x=1").unwrap(), "github.com");
        for refused in [
            "http://github.com/o/r.git",
            "git://github.com/o/r.git",
            "ssh://git@github.com/o/r.git",
            "file:///srv/r.git",
            "ext::sh -c id",
            "/srv/r.git",
            "local",
            "https://user@github.com/o/r.git",
            "https://user:pw@github.com/o/r.git",
            "https://127.0.0.1/o/r.git",
            "https://[::1]/o/r.git",
            "https:///o/r.git",
            "https://evil.example.org\\@github.com/o/r.git",
            "https://github.com\\.evil.example.org/o/r.git",
            "https://github.com:/o/r.git",
            "https://github.com:80a/o/r.git",
            "HTTPS://github.com/o/r.git",
        ] {
            assert!(https_host(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_refused_url_says_why() {
        let why = |url: &str| https_host(url).unwrap_err();
        assert!(why("http://github.com/o/r.git").contains("only used for an https"));
        assert!(why("https://u:p@github.com/o/r.git").contains("login of its own"));
        assert!(why("https://127.0.0.1/o/r.git").contains("not an address"));
        assert!(
            why("https://github.com:/o/r.git").contains("not one a credential may be used for")
        );
        assert!(why("https://github.com:80a/o/r.git").contains("cannot be read"));
        assert!(why("nonsense").contains("cannot be read"));
    }

    #[test]
    fn a_credential_name_is_lowercase_letters_digits_and_hyphens() {
        for good in ["a", "github-sei", "x1-2", &"a".repeat(MAX_CREDENTIAL_NAME)] {
            assert_eq!(credential_name_problem(good), None, "{good}");
        }
        assert!(credential_name_problem("").is_some());
        assert!(credential_name_problem(&"a".repeat(MAX_CREDENTIAL_NAME + 1)).is_some());
        for bad in [
            "Upper",
            "has space",
            "under_score",
            "dot.ted",
            "pipe|x",
            "slash/x",
            "é",
        ] {
            assert!(credential_name_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn a_repository_url_is_made_canonical() {
        let c = |raw: &str| canonical_repository(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert_eq!(
            c("https://github.com/cmu-sei/Polar.git"),
            CanonicalRepository {
                url: "https://github.com/cmu-sei/Polar.git".into(),
                host: "github.com".into(),
                path: vec!["cmu-sei".into(), "Polar.git".into()],
            }
        );
        for (raw, url) in [
            (
                "HTTPS://GitHub.COM:443/cmu-sei/Polar.git#readme",
                "https://github.com/cmu-sei/Polar.git",
            ),
            ("https://github.com/o/r/", "https://github.com/o/r"),
            ("https://github.com/o/../p/r", "https://github.com/p/r"),
            ("https://github.com/o/./r", "https://github.com/o/r"),
            (
                "https://git.example.org:8443/o/r.git",
                "https://git.example.org:8443/o/r.git",
            ),
            (
                "https://gitlab.example.org/group/sub/proj.git",
                "https://gitlab.example.org/group/sub/proj.git",
            ),
        ] {
            assert_eq!(c(raw).url, url, "{raw}");
        }
        assert_eq!(c("HTTPS://GitHub.COM/o/r").host, "github.com");
    }

    #[test]
    fn two_spellings_of_one_repository_are_one_url() {
        let a = canonical_repository("https://GITHUB.com:443/o/r.git").unwrap();
        let b = canonical_repository("https://github.com/o/r.git#frag").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn text_that_is_not_the_https_url_of_a_repository_is_refused() {
        for refused in [
            // not https
            "http://github.com/o/r.git",
            "git://github.com/o/r.git",
            "ssh://git@github.com/o/r.git",
            "git@github.com:o/r.git",
            "file:///srv/r.git",
            "ext::sh -c id",
            "ext::https://github.com/o/r",
            "/srv/r.git",
            "github.com/o/r",
            "",
            // a login
            "https://user@github.com/o/r.git",
            "https://user:pw@github.com/o/r.git",
            "https://github.com@evil.example.org/o/r.git",
            "https://:@github.com/o/r.git",
            // an address, in any spelling
            "https://127.0.0.1/o/r.git",
            "https://2130706433/o/r.git",
            "https://0x7f000001/o/r.git",
            "https://0177.0.0.1/o/r.git",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/o/r.git",
            "https://[::ffff:127.0.0.1]/o/r.git",
            // no host, or an odd one
            "https:///o/r.git",
            "https://github.com./o/r.git",
            "https://github.com:/o/r.git",
            "https://github.com:80a/o/r.git",
            // read two ways by two parsers
            "https://evil.example.org\\@github.com/o/r.git",
            "https://github.com\\.evil.example.org/o/r.git",
            // no repository in it
            "https://github.com",
            "https://github.com/",
            "https://github.com//o/r",
            "https://github.com/o//r",
            // a query, or something hiding in the path
            "https://github.com/o/r.git?service=git-upload-pack",
            "https://github.com/o%2fx/r.git",
            "https://github.com/o/%2e%2e/r",
            "https://github.com/o/r%00.git",
            // whitespace and control characters
            "https://github.com/o/r.git extra",
            "https://github.com/o/r.git\n",
            "https://github.com/o/\tr.git",
            "https://github.com/o/r\u{0}.git",
        ] {
            assert!(canonical_repository(refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_very_long_url_is_refused() {
        let long = format!("https://github.com/o/{}", "r".repeat(MAX_REPO_URL));
        assert!(canonical_repository(&long).unwrap_err().contains("longer"));
        let exact = format!(
            "https://github.com/o/{}",
            "r".repeat(MAX_REPO_URL - "https://github.com/o/".len())
        );
        assert_eq!(exact.len(), MAX_REPO_URL);
        assert!(canonical_repository(&exact).is_ok());
    }

    #[test]
    fn a_lookalike_host_is_a_different_host_and_a_non_ascii_one_is_refused() {
        let c = |raw: &str| canonical_repository(raw).unwrap();
        assert_eq!(
            c("https://github.com.evil.example.org/o/r").host,
            "github.com.evil.example.org"
        );
        assert_eq!(c("https://evilgithub.com/o/r").host, "evilgithub.com");
        // a non-ASCII host is not taken at all: the text and its punycode are two readings of one authority
        assert!(canonical_repository("https://g\u{456}thub.com/o/r").is_err());
    }

    #[test]
    fn a_refused_url_says_which_thing_it_was() {
        let why = |raw: &str| canonical_repository(raw).unwrap_err();
        assert!(why("http://github.com/o/r").contains("https"));
        assert!(why("https://u:p@github.com/o/r").contains("login"));
        assert!(why("https://127.0.0.1/o/r").contains("address"));
        assert!(why("https://github.com/o/r?x=1").contains("query"));
        assert!(why("https://github.com/").contains("repository"));
        assert!(why("https://github.com/o%2fx/r").contains("%"));
        assert!(why("https://github.com/o/r z").contains("space"));
    }

    #[test]
    fn the_repository_id_is_the_same_for_every_spelling_of_a_repository() {
        let id = |raw: &str| canonical_repository(raw).unwrap().repo_id();
        assert_eq!(
            id("https://github.com/o/r.git"),
            id("https://github.com/o/r")
        );
        assert_eq!(
            id("HTTPS://GitHub.com:443/o/r/#x"),
            id("https://github.com/o/r.git")
        );
        assert_ne!(
            id("https://github.com/o/r"),
            id("https://github.com/o/other")
        );
        assert_eq!(
            id("https://github.com/o/r.git").to_string(),
            "https_github.com_o_r"
        );
    }
}
