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

//! What a component reads to find its key: a file, or the custody daemon's socket, exactly one. Declared here once,
//! for every component that may be given the daemon's key: the Cassini client, the broker, the git server and the
//! graph (docs/architecture/key-custody.md, steps 2 and 4).

use polar_env::{Env, EnvError, Optional, Problem};

/// The custody daemon's socket: the daemon in the pod signs this component's TLS 1.3 handshakes with a key the
/// component never holds. The daemon binds the same variable.
pub const CUSTODY_SOCKET: Optional<String> = Optional::text(
    "POLAR_CUSTODY_SOCKET",
    "path to the custody daemon's socket, through which the daemon signs this component's TLS 1.3 handshakes with a key it never holds; this or the key file, not both",
);

/// Where a key is: the file a variable names, or the custody daemon's socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyGiven {
    File(String),
    Socket(String),
}

/// The key `file` names, or the custody socket: exactly one is given. Neither is `file` missing; both is `file` set
/// as well as the socket. `file_doc` is what `file` is for, said with the problem.
pub fn one_key(
    env: &dyn Env,
    file: &Optional<String>,
    file_doc: &'static str,
) -> Result<KeyGiven, EnvError> {
    let problem = |problem| EnvError {
        name: file.name(),
        doc: file_doc,
        problem,
    };
    match (file.read(env)?, CUSTODY_SOCKET.read(env)?) {
        (Some(path), None) => Ok(KeyGiven::File(path)),
        (None, Some(socket)) => Ok(KeyGiven::Socket(socket)),
        (None, None) => Err(problem(Problem::Missing)),
        (Some(path), Some(_)) => Err(problem(Problem::Malformed {
            shown: path,
            reason: format!("is set as well as {}; set one", CUSTODY_SOCKET.name()),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_env::Optional;
    use std::collections::HashMap;

    const KEY_DOC: &str = "path to the key; this or POLAR_CUSTODY_SOCKET, not both";
    const KEY: Optional<String> = Optional::text("A_KEY", KEY_DOC);

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Every way the two can be given (unset, blank, a path): exactly one is, and it is the one read.
    #[test]
    fn exactly_one_of_the_file_and_the_socket_is_given() {
        let states = [None, Some("  "), Some("/given")];
        let given = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
        for file in states {
            for socket in states {
                let mut pairs = Vec::new();
                pairs.extend(file.map(|v| ("A_KEY", v)));
                pairs.extend(socket.map(|v| ("POLAR_CUSTODY_SOCKET", v)));
                match (
                    given(file),
                    given(socket),
                    one_key(&env(&pairs), &KEY, KEY_DOC),
                ) {
                    (true, false, Ok(KeyGiven::File(path))) => assert_eq!(path, "/given"),
                    (false, true, Ok(KeyGiven::Socket(path))) => assert_eq!(path, "/given"),
                    (true, true, Err(error)) | (false, false, Err(error)) => {
                        let said = error.to_string();
                        assert!(said.contains("A_KEY"), "{said}");
                        assert!(said.contains("POLAR_CUSTODY_SOCKET"), "{said}");
                    }
                    (_, _, read) => panic!("{pairs:?}: {read:?}"),
                }
            }
        }
    }
}
