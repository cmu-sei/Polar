use cynic::{impl_scalar, Id};
use chrono::{DateTime, Utc};


#[cynic::schema("gitlab")]
pub mod gitlab {}

// represent timestamps
impl_scalar!(DateTime<Utc>, gitlab::Time);