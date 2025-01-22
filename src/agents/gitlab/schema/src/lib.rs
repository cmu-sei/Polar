use cynic::{impl_scalar, Id};

#[cynic::schema("gitlab")]
pub mod gitlab {}

// impl_scalar!(Id, gitlab::UserID);