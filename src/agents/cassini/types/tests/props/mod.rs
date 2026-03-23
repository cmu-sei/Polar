//! Property tests for cassini-types
//! Follows the exact pattern: use proptest::prelude::*; proptest! { #[test] fn test_name(input in any::<Vec<u8>>()) { ... } }

mod lib;
mod trace;
