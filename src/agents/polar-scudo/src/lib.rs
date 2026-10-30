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

//! Scudo, LLVM's hardened allocator, as a Rust global allocator: every Polar binary's
//! (docs/architecture/broker-baseline.md, "Decided"). build.rs compiles it, under the names `polar_scudo_*`, with
//! config/custom_scudo_config.h. A binary installs it with
//!
//! ```ignore
//! #[global_allocator]
//! static GLOBAL: polar_scudo::Scudo = polar_scudo::Scudo;
//! ```
//!
//! Linking it also gives the binary LLVM libc's memcpy and memmove in place of the libc's (build.rs).

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::{c_int, c_void};

unsafe extern "C" {
    fn polar_scudo_malloc(size: usize) -> *mut c_void;
    fn polar_scudo_calloc(nmemb: usize, size: usize) -> *mut c_void;
    fn polar_scudo_realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
    fn polar_scudo_memalign(alignment: usize, size: usize) -> *mut c_void;
    fn polar_scudo_free(ptr: *mut c_void);
    fn polar_scudo_mallopt(param: c_int, value: c_int) -> c_int;
}

/// The alignment Scudo's `malloc` always gives (`SCUDO_MIN_ALIGNMENT_LOG` is 4 on 64-bit targets), the same as
/// what std's `System` assumes of malloc.
const MIN_ALIGN: usize = 16;

/// Scudo as the global allocator. As std's `System` does with malloc: the plain calls when the alignment is one
/// malloc gives anyway, `memalign` otherwise.
pub struct Scudo;

unsafe impl GlobalAlloc for Scudo {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            if layout.align() <= MIN_ALIGN && layout.align() <= layout.size() {
                polar_scudo_malloc(layout.size()) as *mut u8
            } else {
                polar_scudo_memalign(layout.align(), layout.size()) as *mut u8
            }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe {
            if layout.align() <= MIN_ALIGN && layout.align() <= layout.size() {
                polar_scudo_calloc(1, layout.size()) as *mut u8
            } else {
                let p = self.alloc(layout);
                if !p.is_null() {
                    std::ptr::write_bytes(p, 0, layout.size());
                }
                p
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { polar_scudo_free(ptr as *mut c_void) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            if layout.align() <= MIN_ALIGN && layout.align() <= new_size {
                polar_scudo_realloc(ptr as *mut c_void, new_size) as *mut u8
            } else {
                let new = self.alloc(Layout::from_size_align_unchecked(new_size, layout.align()));
                if !new.is_null() {
                    std::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
                    self.dealloc(ptr, layout);
                }
                new
            }
        }
    }
}

/// Scudo's `mallopt` parameters (compiler-rt/lib/scudo/standalone/include/scudo/interface.h).
pub mod param {
    /// The interval, in milliseconds, before freed memory goes back to the kernel (off Android, `wrappers_c.cpp`
    /// passes the value straight to `Option::ReleaseInterval`); clamped to the configuration's bounds.
    pub const M_DECAY_TIME: i32 = -100;
    /// Give freed memory back to the kernel now.
    pub const M_PURGE: i32 = -101;
    /// Give everything that can be given back to the kernel now.
    pub const M_PURGE_ALL: i32 = -104;
    /// How many freed large blocks the large-block cache keeps.
    pub const M_CACHE_COUNT_MAX: i32 = -200;
    /// The largest freed block the large-block cache keeps.
    pub const M_CACHE_SIZE_MAX: i32 = -201;
    /// How many thread caches threads share (a shared registry only).
    pub const M_TSDS_COUNT_MAX: i32 = -202;
}

/// Sets one of Scudo's options at run time; true when Scudo took it.
pub fn mallopt(param: i32, value: i32) -> bool {
    unsafe { polar_scudo_mallopt(param, value) == 1 }
}
