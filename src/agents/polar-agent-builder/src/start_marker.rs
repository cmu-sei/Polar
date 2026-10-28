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

//! The fresh-start marker (docs/architecture/deployments.md, step 4).
//!
//! A pod restarted in place runs `polar-init` again, which leaves a one-use marker for the agent. The kubelet restarts a
//! container killed by its liveness probe alone, without `polar-init`, and the pod's restart rule does not act on that
//! kill. So the agent takes the marker as it starts: finding none, it was restarted alone, and it exits 1, an exit the
//! rule turns into a restart of the whole pod. The deployment says where the marker is (`POLAR_START_MARKER`); an agent
//! run by hand or in a test is not told, and checks nothing.

use std::io::ErrorKind;

/// Takes the marker at `marker`, if the deployment named one: `Ok` when there is none to take or it was there (and is
/// now gone), why not otherwise.
pub(crate) fn take(marker: Option<&str>) -> Result<(), String> {
    let Some(path) = marker else {
        return Ok(());
    };
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(format!(
            "restarted without polar-init: there is no start marker at {path}, so this container was restarted alone; \
             exiting, so that the pod restarts whole"
        )),
        Err(e) => Err(format!(
            "the start marker at {path} could not be taken: {e}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_marker_named_nothing_is_checked() {
        assert_eq!(take(None), Ok(()));
    }

    #[test]
    fn a_marker_is_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started-by-polar-init");
        std::fs::write(&marker, b"").unwrap();
        let path = marker.display().to_string();
        assert_eq!(take(Some(&path)), Ok(()));
        assert!(!marker.exists());
        let again = take(Some(&path)).unwrap_err();
        assert!(again.contains("polar-init"), "{again}");
    }

    #[test]
    fn no_marker_means_restarted_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("started-by-polar-init")
            .display()
            .to_string();
        let why = take(Some(&path)).unwrap_err();
        assert!(why.contains("restarted without polar-init"), "{why}");
    }
}
