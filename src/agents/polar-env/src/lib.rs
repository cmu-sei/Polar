//! polar-env
//!
//! Declare an environment variable once; read it, validate it and describe it from
//! that one declaration.
//!
//! ```
//! use polar_env::{Defaulted, Env, Required};
//!
//! static ENDPOINT: Required<String> =
//!     Required::text("GRAPH_ENDPOINT", "Bolt address of the Neo4j server");
//! static TICK: Defaulted<u64> =
//!     Defaulted::seconds("POLAR_HEALTH_TICK_SECS", "seconds between health ticks", "30");
//!
//! let env = |name: &str| (name == "GRAPH_ENDPOINT").then(|| "neo4j:7687".to_string());
//! assert_eq!(ENDPOINT.read(&env).unwrap(), "neo4j:7687");
//! assert_eq!(TICK.read(&env).unwrap(), 30);
//! ```
//!
//! Why this exists. A pod fails when an environment variable is missing or wrong, and
//! the message has to say which one and what it is for. `std::env::var("X")?` says only
//! "environment variable not found". And the list of variables a component needs must not
//! exist apart from the code that reads them, or the two drift. A declaration here is the
//! one place the name, type, default, documentation and secrecy live, and:
//!
//! - [`Required::read`] and friends parse it, and fail naming the variable and its purpose;
//! - [`Described::validate`] checks it without needing the value, which is what a
//!   preflight check wants;
//! - [`Described::desc`] describes it, for documentation and for checking that what a
//!   deployment provides covers what the code reads.
//!
//! A component groups its variables into a [`Section`], with any relations between them,
//! and whoever composes components (the agent builder, by capability) checks sections
//! against an [`Env`], which is the process environment at runtime and a map in a test or
//! a preflight check, so the same code judges both.
//!
//! A blank value counts as unset, as it does in Kubernetes manifests that set a name
//! with an empty string. A variable marked secret is never shown in an error or a
//! description.

mod env;
mod error;
mod section;
mod var;

pub use env::{Env, ProcessEnv};
pub use error::{EnvError, Problem};
pub use section::Section;
pub use var::{Defaulted, Described, Kind, Optional, Required, VarDesc};
