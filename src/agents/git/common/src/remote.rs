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
}
