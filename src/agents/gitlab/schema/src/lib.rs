use cynic::impl_scalar;
use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;
use std::fmt;

#[cynic::schema("gitlab")]
pub mod gitlab {}

/// NOTE: Cynic tries to force us to use certain scalars in our rust datatypes if the schema demands it. For example, these few below.
/// For our use of rkyv, we need newtypes that can be serialized easily to bytes by implementing the needed traits.

/// wrap timestamp in newtype string we can serialize to bytes later
#[derive(
    Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive, Default, Clone,
)]
pub struct DateTimeString(String);

impl fmt::Display for DateTimeString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(
    Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive, Clone, Default,
)]
pub struct IdString(pub String);

impl IdString {
    pub fn new<S: Into<String>>(s: S) -> Self {
        IdString(s.into())
    }
}

impl fmt::Display for IdString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(
    Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive, Clone, Default,
)]
pub struct JobIdString(pub String);

impl JobIdString {
    pub fn new<S: Into<String>>(s: S) -> Self {
        JobIdString(s.into())
    }
}

impl fmt::Display for JobIdString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(
    Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive, Clone, Default,
)]
pub struct CiJobArtifactID(pub String);

impl CiJobArtifactID {
    pub fn new<S: Into<String>>(s: S) -> Self {
        CiJobArtifactID(s.into())
    }
}

impl fmt::Display for CiJobArtifactID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
// 
// Represents non-fractional signed whole numeric values. Since the value may exceed the size of a 32-bit integer, it's encoded as a string.
// 
#[derive(
    Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive, Clone, Default,
)]
pub struct BigInt(String);

impl BigInt {
    pub fn new<S: Into<String>>(s: S) -> Self {
        BigInt(s.into())
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}


impl_scalar!(BigInt, gitlab::BigInt);
impl_scalar!(IdString, gitlab::ID);
impl_scalar!(CiJobArtifactID, gitlab::CiJobArtifactID);
impl_scalar!(JobIdString, gitlab::JobID);

// represent timestamps
impl_scalar!(DateTimeString, gitlab::Time);

