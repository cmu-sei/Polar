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

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, Clone)]
pub struct JiraFieldSchema {
    pub custom: Option<String>,
    pub customId: Option<f64>,
    pub items: Option<String>,
    pub system: Option<String>,
    pub r#type: String,
}

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, Clone)]
pub struct JiraField {
    pub clauseNames: Option<Vec<String>>,
    pub custom: bool,
    pub id: String,
    //pub key: String,
    pub name: String,
    pub navigable: bool,
    pub orderable: bool,
    pub schema: Option<JiraFieldSchema>,
    pub searchable: bool,
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
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum FourthTierField {
    Object(HashMap<String, FifthTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum ThirdTierField {
    Object(HashMap<String, FourthTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum SecondTierField {
    Object(HashMap<String, ThirdTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize,Debug)]
#[serde(untagged)]
pub enum FirstTierField {
    Object(HashMap<String, SecondTierField>),
    Bool(bool),
    Number(f64),
    List(Vec<SecondTierField>),
    Option(Option<String>),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum NestedListTypes {
    Object(HashMap<String, FirstTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, Debug)]
#[serde(untagged)]
pub enum FieldValue {
    Object(HashMap<String, FirstTierField>),
    OptionValue(Option<String>),
    Number(f64),
    Bool(bool),
    List(Vec<NestedListTypes>),
    Null,
}

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
    Issues(JsonString),
}

/// Helper type to link connection types to a resource's id
/// For example, a user or group to projects, or a group to users, etc.
#[derive(Debug, Serialize, Deserialize, Archive)]
pub struct ResourceLink<T> {
    pub resource_id: IdString,
    pub connection: T,
}
