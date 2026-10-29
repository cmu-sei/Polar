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
