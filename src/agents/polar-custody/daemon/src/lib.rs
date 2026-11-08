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

//! The custody daemon beyond its socket (docs/architecture/key-custody.md, step 5 and "Part A's bootstrap: the
//! custodian"): as its custodian, `custody.<X>`, it fetches what its workload needs and watches for revocation, and
//! destroys its keys when it sees one of its certificates revoked.
//!
//! - [`bootstrap`]: what the custodian fetches, and what each reply means for the custody.
//! - [`watch`]: what the credentials agent's authority means for the keys held, and the actor that is the custodian's
//!   session.

pub mod bootstrap;
pub mod watch;

use polar_custody::custody::Custody;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// How often the daemon looks whether its custodian has failed.
pub const FAILURE_LOOK_EVERY: Duration = Duration::from_secs(1);

/// How long a daemon whose custodian failed waits before it ends: long enough for polar-init, which asks whether it is
/// ready every half second, to be told why.
pub const FAILED_GRACE: Duration = Duration::from_secs(5);

/// Ends the daemon once its custodian has failed (docs/architecture/key-custody.md, "A retry in place"). What it holds
/// is spent: the credentials agent answers a custodian's certificate once, and its slots are sealed. polar-init stops
/// when it is told, and runs again in the same pod with this daemon still there, a native sidecar not being restarted
/// with an init container; so the daemon ends, as it does when revoked, and the kubelet starts it again empty, for
/// polar-init's next run to start from. `end` is called once, `grace` after the failure was first seen, with why; it is
/// looked for every `every`.
pub async fn end_once_failed(
    custody: Arc<RwLock<Custody>>,
    every: Duration,
    grace: Duration,
    end: impl FnOnce(String),
) {
    loop {
        tokio::time::sleep(every).await;
        let failure = custody.read().ok().and_then(|custody| custody.failure());
        if let Some(why) = failure {
            tokio::time::sleep(grace).await;
            end(why);
            return;
        }
    }
}

/// Locks the process's memory, so that no key it holds reaches swap: every page mapped now and every page mapped later,
/// each as it is first touched (`MCL_ONFAULT`). Without that the kernel backs a mapping the moment it is made, and an
/// allocator's untouched reservation (mimalloc's arena is 1 GiB) becomes resident memory that can never be reclaimed.
pub fn lock_memory() -> std::io::Result<()> {
    // SAFETY: mlockall takes no pointers; it changes only this process.
    let locked =
        unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE | libc::MCL_ONFAULT) };
    if locked == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn a_custody() -> Arc<RwLock<Custody>> {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        Arc::new(RwLock::new(Custody::new(&pem).unwrap()))
    }

    const EVERY: Duration = Duration::from_millis(10);
    const GRACE: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn a_daemon_whose_custodian_failed_ends_once_polar_init_has_had_time_to_hear_why() {
        let custody = a_custody();
        let ended = Arc::new(Mutex::new(None));
        let told = ended.clone();
        let watching = tokio::spawn(end_once_failed(custody.clone(), EVERY, GRACE, move |why| {
            *told.lock().unwrap() = Some(why);
        }));
        tokio::time::sleep(GRACE).await;
        assert_eq!(*ended.lock().unwrap(), None, "nothing failed");
        custody
            .write()
            .unwrap()
            .set_failed("the credentials agent refused".to_string());
        tokio::time::sleep(GRACE / 2).await;
        assert_eq!(*ended.lock().unwrap(), None, "not before the grace");
        watching.await.unwrap();
        assert_eq!(
            ended.lock().unwrap().as_deref(),
            Some("the credentials agent refused")
        );
    }

    #[tokio::test]
    async fn a_daemon_whose_custodian_fetched_everything_does_not_end() {
        let custody = a_custody();
        custody.write().unwrap().set_ready();
        // Once ready, a failure cannot be said: the workload may be running on what it holds.
        custody.write().unwrap().set_failed("late".to_string());
        let ended = Arc::new(Mutex::new(false));
        let told = ended.clone();
        let watching = tokio::spawn(end_once_failed(custody, EVERY, GRACE, move |_| {
            *told.lock().unwrap() = true;
        }));
        tokio::time::sleep(GRACE * 3).await;
        assert!(!*ended.lock().unwrap());
        watching.abort();
    }
}
