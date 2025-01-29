use cynic::impl_scalar;
use rkyv::Serialize;
use rkyv::Deserialize;
use rkyv::Archive;

#[cynic::schema("gitlab")]
pub mod gitlab {}

/// NOTE: Cynic tries to force us to use certain scalars in our rust datatypes if the schema demands it. For example, these few below.
/// For our use of rkyv, we need newtypes that can be serialized easily to bytes by implementing the needed traits.

/// wrap timestamp in newtype string we can serialize to bytes later
#[derive (Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive)]
pub struct DateTimeString(String);
#[derive (Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive)]
pub struct IdString(String);

impl_scalar!(IdString, gitlab::ID);

// represent timestamps
impl_scalar!(DateTimeString, gitlab::Time);