/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use rkyv::{Archive, Deserialize, Serialize, };
use rkyv::bytecheck::CheckBytes;
use std::collections::HashMap;

use serde::Deserialize as SerdeDeserialize;

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, Clone)]
pub struct JiraProject {
    pub id: String,
    pub key: String,
    pub name: String,
    pub projectTypeKey: String,
}

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, Clone)]
pub struct JiraGroup {
    //pub groupId: String,
    pub name: String,
    pub html: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, Clone)]
pub struct JiraUser {
    pub name: String,
    pub key: String,
    pub html: Option<String>,
    pub displayName: String,
}

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, Clone, Debug)]
pub struct JiraAuthor {
    pub name: String,
    pub key: String,
    pub emailAddress: String,
    pub avatarUrls: HashMap<String, String>,
    pub displayName: String,
    pub active: bool,
    pub timeZone: String,
}

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, Clone, Debug)]
pub struct JiraItemHistory {
    pub field: String,
    pub fieldtype: String,
    pub from: Option<String>,
    pub fromString: Option<String>,
    pub to: Option<String>,
    pub toString: Option<String>,
}

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, Clone, Debug)]
pub struct JiraHistory {
    pub id: String,
    pub author: JiraAuthor,
    pub created: String,
    pub items: Vec<JiraItemHistory>,
}

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, Clone, Debug)]
pub struct JiraIssueChangeLog {
    pub startAt: f64,
    pub maxResults: f64,
    pub total: f64,
    pub histories: Vec<JiraHistory>,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum FifthTierField {
    Option(String),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum FourthTierField {
    Object(HashMap<String, Option<FifthTierField>>),
    Option(String),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum ThirdTierField {
    Object(HashMap<String, Option<FourthTierField>>),
    Option(String),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum SecondTierField {
    Object(HashMap<String, Option<ThirdTierField>>),
    Option(String),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize,Debug)]
#[serde(untagged)]
pub enum FirstTierField {
    Object(HashMap<String, Option<SecondTierField>>),
    Option(String),
    Bool(bool),
    Number(f64),
    List(Vec<SecondTierField>),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum NestedListTypes {
    Object(HashMap<String, Option<FirstTierField>>),
    Option(String),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum FieldValue {
    Object(HashMap<String, Option<FirstTierField>>),
    Option(String),
    Number(f64),
    Bool(bool),
    List(Vec<NestedListTypes>),
    Null,
}

/*
#[derive(Debug, Archive, SerdeDeserialize, Serialize, Deserialize, Clone)]
pub enum FlatValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<String>), // Flattened
    Object(HashMap<String, String>), // Flattened
}

#[derive(Debug, Archive, SerdeDeserialize, Serialize, Deserialize, Clone)]
pub struct GenericJson {
    pub fields: HashMap<String, FlatValue>,
}

impl From<serde_json::Value> for GenericJson {
    fn from(value: serde_json::Value) -> Self {
        let mut fields = HashMap::new();
        if let serde_json::Value::Object(map) = value {
            for (k, v) in map {
                fields.insert(k, FlatValue::from(v));
            }
        }
        GenericJson { fields }
    }
}

impl From<serde_json::Value> for FlatValue {
    fn from(value: serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => FlatValue::Null,
            serde_json::Value::Bool(b) => FlatValue::Bool(b),
            serde_json::Value::Number(n) => FlatValue::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) => FlatValue::String(s),
            serde_json::Value::Array(arr) => {
                FlatValue::Array(arr.into_iter().map(|v| serde_json::to_string(&v).unwrap_or_default()).collect())
            }
            serde_json::Value::Object(obj) => {
                FlatValue::Object(obj.into_iter().map(|(k, v)| (k, serde_json::to_string(&v).unwrap_or_default())).collect())
            }
        }
    }
}
*/

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, Clone, Debug)]
pub struct JiraIssue {
    pub expand: String,
    pub id: String,
    #[serde(rename = "self")]
    pub self_url: String,
    pub key: String,
    pub fields: HashMap<String, FieldValue>,
    pub renderedFields: Option<String>,
    pub changelog: JiraIssueChangeLog,
}

#[derive(Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive, Clone, Default,)]
pub struct IdString(pub String);

#[derive(Debug, Serialize, Deserialize, serde::Deserialize, serde::Serialize, Archive, Clone, Default,)]
pub struct JsonString {
    pub json: String
}


/// This enum mostly serves as a way to inform the deserializer what datatype to map the bytes into.
/// The underlying byte vector contains a message meant for some consumer on a given topic
#[derive(Serialize, Deserialize, Archive, SerdeDeserialize, Debug)]
pub enum JiraData {
    Projects(Vec<JiraProject>),
    Groups(Vec<JiraGroup>),
    Users(Vec<JiraUser>),
    //Issues(Vec<JiraIssue>),
    Issues(JsonString),
}

/// Helper type to link connection types to a resource's id
/// For example, a user or group to projects, or a group to users, etc.
#[derive(Debug, Serialize, Deserialize, Archive)]
pub struct ResourceLink<T> {
    pub resource_id: IdString,
    pub connection: T,
}
