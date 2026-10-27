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

//! The custody daemon, as the health actor sees it (docs/architecture/key-custody.md, step 4, item 6): a daemon that
//! restarted is empty, no key and no certificate, and the pod can make no new connection, ever. A slot once installed
//! never changes (it is sealed), so holding a certificate in either slot is the whole of the check.

use polar_custody::client::{ClientError, CustodyClient};
use polar_custody::protocol::{Refusal, Slot};
use std::time::Duration;

/// How long the health actor waits on the daemon in a tick.
const ASK_WITHIN: Duration = Duration::from_secs(2);

/// What the daemon was found holding this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Custody {
    /// The pod has no daemon: its keys are files.
    NotUsed,
    /// A certificate is installed in one of its slots.
    Holds,
    /// No slot holds a certificate: the daemon restarted.
    Empty,
    /// It could not be asked, and why.
    Unreachable(String),
}

/// What the daemon at `socket` holds. Blocking: the actor asks it off its own thread.
pub(crate) fn read(socket: &str) -> Custody {
    let daemon = CustodyClient::new(socket).with_timeout(ASK_WITHIN);
    let mut empty = 0;
    for slot in [Slot::Client, Slot::Server] {
        match daemon.certificate(slot) {
            Ok(_) => return Custody::Holds,
            Err(ClientError::Refused(Refusal::NoKey | Refusal::NoCertificate)) => empty += 1,
            Err(e) => return Custody::Unreachable(e.to_string()),
        }
    }
    debug_assert_eq!(empty, 2);
    Custody::Empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_custody::custody::Custody as Daemon;
    use polar_custody::server::{Callers, bind, serve};
    use std::os::unix::fs::MetadataExt as _;
    use std::sync::{Arc, RwLock};

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_daemon_a_daemon_holding_a_certificate_and_none_at_all() {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let ca_pem = params.self_signed(&key).unwrap().pem();
        let issuer = rcgen::Issuer::new(params, key);
        let dir = tempfile::tempdir().unwrap();
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        let socket = dir.path().join("custody.sock");
        tokio::spawn(serve(
            bind(&socket).unwrap(),
            Arc::new(RwLock::new(Daemon::new(&ca_pem).unwrap())),
            Arc::new(Callers {
                installer_uid: uid,
                workload_uids: vec![uid],
            }),
        ));
        let at = socket.display().to_string();
        let ask = |at: String| tokio::task::spawn_blocking(move || read(&at));

        assert_eq!(ask(at.clone()).await.unwrap(), Custody::Empty);

        let daemon = CustodyClient::new(&socket);
        let csr = tokio::task::block_in_place(|| {
            daemon.make_csr(
                Slot::Server,
                "cassini.polar.serviceaccount.cluster.local",
                &[],
            )
        })
        .unwrap();
        let request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        let certificate = request.signed_by(&issuer).unwrap().pem();
        tokio::task::block_in_place(|| daemon.install(Slot::Server, vec![certificate])).unwrap();
        assert_eq!(
            ask(at).await.unwrap(),
            Custody::Holds,
            "the server slot alone"
        );

        let missing = dir.path().join("none.sock").display().to_string();
        assert!(matches!(
            ask(missing).await.unwrap(),
            Custody::Unreachable(_)
        ));
    }
}
