/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone)]
pub struct JiraProject {
    pub id: String,
    pub key: String,
    pub name: String,
    #[serde(rename = "projectTypeKey")]
    pub project_type_key: String,
}

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone)]
pub struct JiraGroup {
    //pub groupId: String,
    pub name: String,
    pub html: Option<String>,
    pub label: Option<String>,
}

/// A user as the observer lists them. Every identity field may be missing: a site decides what it shows, and a Data
/// Center can hide any of them from the account the observer reads as (docs/architecture/jira.md, "The review before
/// 3.0.0", 2).
#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone)]
pub struct JiraUser {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
}

impl JiraUser {
    /// What the user's node is keyed by: its `key`, or its `name` where the site gives no key. `None`: the site gave
    /// neither, and the user cannot be written.
    pub fn identity(&self) -> Option<&str> {
        non_empty(&self.key).or_else(|| non_empty(&self.name))
    }
}

/// The text of `field` unless it is missing or empty.
fn non_empty(field: &Option<String>) -> Option<&str> {
    field.as_deref().filter(|s| !s.is_empty())
}

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone)]
pub struct JiraFieldSchema {
    pub custom: Option<String>,
    #[serde(rename = "customId")]
    pub custom_id: Option<f64>,
    pub items: Option<String>,
    pub system: Option<String>,
    pub r#type: String,
}

#[derive(Debug, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone)]
pub struct JiraField {
    #[serde(rename = "clauseNames")]
    pub clause_names: Option<Vec<String>>,
    pub custom: bool,
    pub id: String,
    //pub key: String,
    pub name: String,
    pub navigable: bool,
    pub orderable: bool,
    pub schema: Option<JiraFieldSchema>,
    pub searchable: bool,
}

/// The author of a change in an issue's history. Only what says who it was is used, and every field may be missing:
/// a site that hides email addresses sends none (Apache's Jira, anonymously: `name`, `key`, `displayName`,
/// `avatarUrls`, `active`, `timeZone` and `self`), and a Data Center can hide the others too.
#[derive(
    Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone, Debug, Default,
)]
pub struct JiraAuthor {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    /// Jira Cloud's only identity for a user.
    #[serde(default, rename = "accountId")]
    pub account_id: Option<String>,
    #[serde(default, rename = "emailAddress")]
    pub email_address: Option<String>,
    #[serde(default, rename = "avatarUrls")]
    pub avatar_urls: HashMap<String, String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default, rename = "timeZone")]
    pub time_zone: Option<String>,
}

impl JiraAuthor {
    /// Who the author was, as a changelog entry records it: the user's `key`, else `name`, else Cloud's `accountId`;
    /// `None` when the site said none of them.
    pub fn identity(&self) -> Option<&str> {
        non_empty(&self.key)
            .or_else(|| non_empty(&self.name))
            .or_else(|| non_empty(&self.account_id))
    }
}

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone, Debug)]
pub struct JiraItemHistory {
    pub field: String,
    pub fieldtype: String,
    pub from: Option<String>,
    #[serde(rename = "fromString")]
    pub from_string: Option<String>,
    pub to: Option<String>,
    #[serde(rename = "toString")]
    pub to_string: Option<String>,
}

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone, Debug)]
pub struct JiraHistory {
    pub id: String,
    pub author: Option<JiraAuthor>,
    pub created: String,
    pub items: Vec<JiraItemHistory>,
}

#[derive(
    Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone, Debug, Default,
)]
pub struct JiraIssueChangeLog {
    #[serde(rename = "startAt")]
    pub start_at: f64,
    #[serde(rename = "maxResults")]
    pub max_results: f64,
    pub total: f64,
    pub histories: Vec<JiraHistory>,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Debug)]
#[serde(untagged)]
pub enum FifthTierField {
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Debug)]
#[serde(untagged)]
pub enum FourthTierField {
    Object(HashMap<String, FifthTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Debug)]
#[serde(untagged)]
pub enum ThirdTierField {
    Object(HashMap<String, FourthTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Debug)]
#[serde(untagged)]
pub enum SecondTierField {
    Object(HashMap<String, ThirdTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Debug)]
#[serde(untagged)]
pub enum FirstTierField {
    Object(HashMap<String, SecondTierField>),
    Bool(bool),
    Number(f64),
    List(Vec<SecondTierField>),
    Option(Option<String>),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Debug)]
#[serde(untagged)]
pub enum NestedListTypes {
    Object(HashMap<String, FirstTierField>),
    Option(Option<String>),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Clone, Archive, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Debug)]
#[serde(untagged)]
pub enum FieldValue {
    Object(HashMap<String, FirstTierField>),
    OptionValue(Option<String>),
    Number(f64),
    Bool(bool),
    List(Vec<NestedListTypes>),
    Null,
}

#[derive(Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize, Clone, Debug)]
pub struct JiraIssue {
    #[serde(default)]
    pub expand: String,
    pub id: String,
    #[serde(rename = "self")]
    pub self_url: String,
    pub key: String,
    pub fields: HashMap<String, FieldValue>,
    #[serde(rename = "renderedFields")]
    pub rendered_fields: Option<String>,
    /// An issue without its history (a search that did not expand it) is still an issue, with an empty one.
    #[serde(default)]
    pub changelog: JiraIssueChangeLog,
    /// The value of the system field `updated`, as the observer found it ([`crate::SYSTEM_UPDATED_KEY`]); `None` from
    /// an observer that did not say, or for an issue without it.
    #[serde(default, rename = "polar_updated")]
    pub system_updated: Option<String>,
}

#[derive(
    Debug, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Archive, Clone, Default,
)]
pub struct IdString(pub String);

#[derive(
    Debug, Serialize, Deserialize, SerdeDeserialize, SerdeSerialize, Archive, Clone, Default,
)]
pub struct JsonString {
    pub json: String,
}
/// This enum mostly serves as a way to inform the deserializer what datatype to map the bytes into.
/// The underlying byte vector contains a message meant for some consumer on a given topic
#[derive(Serialize, Deserialize, Archive, serde::Deserialize, serde::Serialize, Debug)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A changelog author exactly as Apache's Jira (Data Center 10.3, anonymously) sends one: no email.
    fn apache_author() -> serde_json::Value {
        json!({
            "self": "https://issues.apache.org/jira/rest/api/2/user?username=rgardler",
            "name": "rgardler",
            "key": "rgardler",
            "avatarUrls": {
                "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
                "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452"
            },
            "displayName": "Ross Gardler",
            "active": true,
            "timeZone": "America/Los_Angeles"
        })
    }

    #[test]
    fn an_author_without_an_email_decodes_and_says_who_it_was() {
        let author: JiraAuthor = serde_json::from_value(apache_author()).unwrap();
        assert_eq!(author.email_address, None);
        assert_eq!(author.identity(), Some("rgardler"));
        assert_eq!(author.display_name.as_deref(), Some("Ross Gardler"));
    }

    #[test]
    fn an_author_the_site_hides_more_of_still_decodes() {
        for (given, identity) in [
            (json!({ "name": "jdoe" }), Some("jdoe")),
            (
                json!({ "key": "JIRAUSER10100", "name": "jdoe" }),
                Some("JIRAUSER10100"),
            ),
            (json!({ "key": "", "name": "jdoe" }), Some("jdoe")),
            (
                json!({ "accountId": "5b10a2844c20165700ede21g" }),
                Some("5b10a2844c20165700ede21g"),
            ),
            (json!({ "displayName": "Someone", "active": false }), None),
            (json!({}), None),
        ] {
            let author: JiraAuthor = serde_json::from_value(given.clone()).unwrap();
            assert_eq!(author.identity(), identity, "{given}");
        }
    }

    #[test]
    fn a_history_with_an_apache_author_decodes() {
        let history: JiraHistory = serde_json::from_value(json!({
            "id": "12793186",
            "author": apache_author(),
            "created": "2009-12-04T12:22:31.383+0000",
            "items": [{ "field": "status", "fieldtype": "jira", "from": "1", "fromString": "Open",
                        "to": "6", "toString": "Closed" }]
        }))
        .unwrap();
        assert_eq!(history.author.unwrap().identity(), Some("rgardler"));
    }

    #[test]
    fn a_user_decodes_with_what_the_site_shows_and_is_keyed_by_key_then_name() {
        for (given, identity) in [
            (
                json!({ "name": "jdoe", "key": "JIRAUSER10100", "displayName": "Jane Doe", "html": "Jane" }),
                Some("JIRAUSER10100"),
            ),
            (
                json!({ "name": "jdoe", "displayName": "Jane Doe" }),
                Some("jdoe"),
            ),
            (json!({ "displayName": "Jane Doe" }), None),
            (json!({ "accountId": "5b10a2844c20165700ede21g" }), None),
        ] {
            let user: JiraUser = serde_json::from_value(given.clone()).unwrap();
            assert_eq!(user.identity(), identity, "{given}");
        }
    }

    #[test]
    fn an_issue_without_its_history_decodes_with_an_empty_one() {
        let issue: JiraIssue = serde_json::from_value(json!({
            "id": "1", "self": "https://jira.example.org/rest/api/2/issue/1", "key": "PLAT-1", "fields": {}
        }))
        .unwrap();
        assert!(issue.changelog.histories.is_empty());
    }
}
