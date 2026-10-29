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

//! The daemon's memory lock (`lock_memory`): every page it touches is locked, so no key reaches swap, and a mapping it has
//! not touched costs nothing. With `MCL_FUTURE` alone the kernel backs every mapping when it is made: mimalloc's 1 GiB
//! arena, untouched, made each daemon on musl hold 1.06 GiB (found on u7c, 2026-10-29). Its own test binary: the lock is
//! the whole process's.

use std::ptr;

const MIB: usize = 1 << 20;

/// A field of /proc/self/status, in KiB ("VmRSS:   1234 kB").
fn status_kib(field: &str) -> usize {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix(field)?.strip_prefix(':'))
        .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse().ok())
        .unwrap_or_else(|| panic!("no {field} in /proc/self/status"))
}

/// The resident pages that are locked in the mapping that starts at `start`, in KiB (its "Locked:" in /proc/self/smaps).
fn locked_kib_in(start: usize) -> usize {
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
    let head = format!("{start:x}-");
    let mut lines = smaps.lines().skip_while(|l| !l.starts_with(&head));
    assert!(lines.next().is_some(), "no mapping at {start:x}");
    // The first Locked: after the mapping's own line is its own: each mapping has one, before the next mapping's line.
    lines
        .find_map(|l| l.strip_prefix("Locked:"))
        .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse().ok())
        .expect("no Locked: in the mapping")
}

#[test]
fn a_mapping_is_not_backed_until_touched_and_what_is_touched_is_locked() {
    if let Err(e) = polar_custody_daemon::lock_memory() {
        // A memory-lock limit below what the process has (an unprivileged default): the daemon logs this and runs on.
        eprintln!(
            "skipped: the process's memory cannot be locked here ({e}); raise the memory-lock limit to run this"
        );
        return;
    }
    const SIZE: usize = 256 * MIB;
    let before = status_kib("VmRSS");
    // SAFETY: an anonymous private mapping, unmapped below; nothing else refers to it.
    let p = unsafe {
        libc::mmap(
            ptr::null_mut(),
            SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(p, libc::MAP_FAILED, "{}", std::io::Error::last_os_error());
    let grown = status_kib("VmRSS").saturating_sub(before);
    assert!(
        grown < 16 * 1024,
        "an untouched {} MiB mapping made the process {} MiB larger",
        SIZE / MIB,
        grown / 1024
    );

    // Touched, its pages are resident and locked: a key written there cannot be swapped out.
    const TOUCHED: usize = 4 * MIB;
    // SAFETY: within the mapping made above.
    unsafe { ptr::write_bytes(p as *mut u8, 0xa5, TOUCHED) };
    let locked = locked_kib_in(p as usize);
    assert!(
        locked >= TOUCHED / 1024,
        "{} KiB touched, {locked} KiB of it locked",
        TOUCHED / 1024
    );

    // SAFETY: the mapping made above, no longer used.
    unsafe { libc::munmap(p, SIZE) };
}
