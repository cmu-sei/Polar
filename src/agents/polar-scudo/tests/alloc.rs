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

// The allocator, as the global allocator of this test binary, so every allocation here (proptest's own too) goes
// through it.

use polar_scudo::{Scudo, mallopt, param};
use proptest::prelude::*;
use std::alloc::{GlobalAlloc, Layout};

#[global_allocator]
static GLOBAL: Scudo = Scudo;

fn layout() -> impl Strategy<Value = Layout> {
    // Sizes from nothing to past the large-block threshold, alignments past what malloc gives on its own.
    (0usize..(3 << 20), 0u32..13)
        .prop_map(|(size, a)| Layout::from_size_align(size, 1 << a).unwrap())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn an_allocation_has_the_alignment_it_asked_for(l in layout()) {
        let p = unsafe { GLOBAL.alloc(l) };
        prop_assert!(!p.is_null());
        prop_assert_eq!(p as usize % l.align(), 0);
        unsafe { GLOBAL.dealloc(p, l) };
    }

    #[test]
    fn a_zeroed_allocation_is_zeros(l in layout()) {
        // Dirty a block of the same layout first, so a reused one would show.
        unsafe {
            let d = GLOBAL.alloc(l);
            std::ptr::write_bytes(d, 0xa5, l.size());
            GLOBAL.dealloc(d, l);
        }
        let p = unsafe { GLOBAL.alloc_zeroed(l) };
        prop_assert!(!p.is_null());
        prop_assert_eq!(p as usize % l.align(), 0);
        let bytes = unsafe { std::slice::from_raw_parts(p, l.size()) };
        prop_assert!(bytes.iter().all(|b| *b == 0));
        unsafe { GLOBAL.dealloc(p, l) };
    }

    #[test]
    fn realloc_keeps_the_contents_and_the_alignment(l in layout(), new_size in 0usize..(3 << 20)) {
        let p = unsafe { GLOBAL.alloc(l) };
        prop_assert!(!p.is_null());
        for i in 0..l.size() {
            unsafe { *p.add(i) = (i % 251) as u8 };
        }
        let q = unsafe { GLOBAL.realloc(p, l, new_size) };
        prop_assert!(!q.is_null());
        prop_assert_eq!(q as usize % l.align(), 0);
        let kept = l.size().min(new_size);
        let bytes = unsafe { std::slice::from_raw_parts(q, kept) };
        prop_assert!(bytes.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
        unsafe { GLOBAL.dealloc(q, Layout::from_size_align(new_size, l.align()).unwrap()) };
    }
}

#[test]
fn threads_allocate_and_free_each_others_blocks() {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let makers: Vec<_> = (0..8)
        .map(|t| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for i in 0..2_000 {
                    tx.send(vec![t as u8; (i * 37) % 70_000]).unwrap();
                }
            })
        })
        .collect();
    drop(tx);
    let mut n = 0;
    for v in rx {
        assert!(v.iter().all(|b| *b == v.first().copied().unwrap_or(0)));
        n += 1;
    }
    makers.into_iter().for_each(|m| m.join().unwrap());
    assert_eq!(n, 16_000);
}

#[test]
fn scudo_takes_the_options_its_configuration_has() {
    // Every configuration has a release interval and a large-block cache.
    assert!(mallopt(param::M_DECAY_TIME, 1000));
    assert!(mallopt(param::M_CACHE_SIZE_MAX, 4 << 20));
    assert!(mallopt(param::M_PURGE, 0));
    // Only a shared registry has a count of thread caches to set.
    let shared = std::env::var("POLAR_SCUDO_CONFIG").as_deref() == Ok("lean");
    assert_eq!(mallopt(param::M_TSDS_COUNT_MAX, 4), shared);
}
