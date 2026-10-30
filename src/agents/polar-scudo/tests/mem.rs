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

// The memcpy and memmove every binary calls are LLVM libc's, which polar-scudo links whole (build.rs): musl's x86_64
// memcpy is `rep movsq` and a byte loop, and took 6% of the broker's time (docs/architecture/broker-baseline.md, "The
// fourth set"). The calls here are to the C symbols, not through `ptr::copy`, so they reach the linked functions.

use polar_scudo::Scudo;
use proptest::prelude::*;
use std::ffi::c_void;

#[global_allocator]
static GLOBAL: Scudo = Scudo;

unsafe extern "C" {
    fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    // LLVM libc's own names for them (namespace __llvm_libc_polar, workspace.nix): `memcpy` and `memmove` are
    // aliases of these in its objects.
    #[link_name = "_ZN17__llvm_libc_polar6memcpyEPvPKvm"]
    fn llvm_libc_memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    #[link_name = "_ZN17__llvm_libc_polar7memmoveEPvPKvm"]
    fn llvm_libc_memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
}

#[test]
fn memcpy_and_memmove_are_llvm_libcs() {
    assert_eq!(
        memcpy as *const () as usize, llvm_libc_memcpy as *const () as usize,
        "memcpy is not LLVM libc's"
    );
    assert_eq!(
        memmove as *const () as usize, llvm_libc_memmove as *const () as usize,
        "memmove is not LLVM libc's"
    );
}

/// The `i`th byte of the test data: never the guard. The expected results are computed from this, not copied, since a
/// copy (`clone`, `to_vec`) is itself a call to memcpy.
fn byte(i: usize, seed: u8) -> u8 {
    (i as u8).wrapping_mul(31).wrapping_add(seed) | 1
}

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| byte(i, seed)).collect()
}

const GUARD: u8 = 0;

fn len() -> impl Strategy<Value = usize> {
    // The sizes each copy strategy takes (LLVM libc's x86_64 memcpy branches at 16, 32, 64, 128 and 256 bytes, and
    // loops above), the broker's message sizes, and past them.
    prop_oneof![0usize..=300, 0usize..=8192, 0usize..=(140 << 10)]
}

proptest! {
    // memcpy copies exactly n bytes, from any source offset to any destination offset, and writes nothing else.
    #[test]
    fn memcpy_copies_exactly_n_bytes(n in len(), src_off in 0usize..64, dst_off in 0usize..64, seed: u8) {
        let src = data(src_off + n, seed);
        let mut dst = vec![GUARD; dst_off + n + 64];
        let ret = unsafe {
            memcpy(dst.as_mut_ptr().add(dst_off).cast(), src.as_ptr().add(src_off).cast(), n)
        };
        prop_assert_eq!(ret as usize, dst.as_ptr() as usize + dst_off);
        prop_assert!((0..n).all(|i| dst[dst_off + i] == byte(src_off + i, seed)));
        prop_assert!(dst[..dst_off].iter().chain(&dst[dst_off + n..]).all(|&b| b == GUARD));
    }

    // memmove within one buffer, the ranges overlapping in either direction or not at all, leaves the buffer as if
    // the source had been copied out first and then written to the destination.
    #[test]
    fn memmove_is_a_copy_through_a_temporary(n in len(), src_off in 0usize..4096, dst_off in 0usize..4096, seed: u8) {
        let mut buf = data(src_off.max(dst_off) + n + 64, seed);
        let expected: Vec<u8> = (0..buf.len())
            .map(|j| if (dst_off..dst_off + n).contains(&j) { byte(src_off + j - dst_off, seed) } else { byte(j, seed) })
            .collect();
        let base = buf.as_mut_ptr();
        let ret = unsafe { memmove(base.add(dst_off).cast(), base.add(src_off).cast(), n) };
        prop_assert_eq!(ret as usize, base as usize + dst_off);
        prop_assert!(buf == expected, "memmove of {} bytes from {} to {} differs", n, src_off, dst_off);
    }
}
