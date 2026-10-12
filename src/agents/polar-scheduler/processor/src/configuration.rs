//! Signed configuration documents, as the scheduler processor meets them: read for where they are going
//! (shared with the observer, in `polar_scheduler_common::routing`), written to the graph, and forwarded
//! untouched.

pub use polar_scheduler_common::routing::{Routing, Shape, classify};
