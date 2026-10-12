//! A pair of throwaway agents that check the credentials agent's secret policy from inside the cluster:
//! one stores a secret and keeps retrieving it, the other, with a different identity, tries to get it or
//! take its name. Neither exists to do work; the results are lines in their logs.

#![deny(clippy::disallowed_methods)]

pub mod agent;
pub mod scenario;
pub mod settings;
pub mod supervisor;
