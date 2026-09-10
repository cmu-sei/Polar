// cert_issuer_init/src/keypair.rs
//
// Moved to cert-issuer-common -- see its own module doc comment on
// generate_csr for why. Re-exported here so every existing caller in
// this crate (main.rs, handshake.rs, keypair_tests.rs) keeps working
// unchanged.

pub use cert_issuer_common::keypair::{CsrOutput, KeyAlgorithm, KeypairError, generate_csr};
