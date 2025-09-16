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

use crate::{subscribe_to_topic, JiraConsumerArgs, JiraConsumerState};
use polar::{QUERY_COMMIT_FAILED, QUERY_RUN_FAILED, TRANSACTION_FAILED_ERROR};
use std::collections::HashMap;

use jira_common::types::JiraData;
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use neo4rs::Query;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info};
use jira_common::types::{
    JiraIssue, JsonString,
    JiraField, JiraFieldSchema,
    FieldValue, FirstTierField,
    NestedListTypes,
    };

pub struct JiraIssueConsumer;

#[async_trait]
impl Actor for JiraIssueConsumer {
    type Msg = JiraData;
    type State = JiraConsumerState;
    type Arguments = JiraConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: JiraConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        match subscribe_to_topic(
            args.registration_id,
            JIRA_ISSUES_CONSUMER_TOPIC.to_string(),
            args.graph_config,
        )
        .await
        {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error starting actor: \"{JIRA_ISSUES_CONSUMER_TOPIC}\" {e}");
                Err(ActorProcessingErr::from(err_msg))
            }
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{:?} waiting to consume", myself.get_name());

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {

        // Declare function to add the 2 user cyphers
        fn merge_user(
            cypher: &mut String,
            role: &str,
            field: Option<&FieldValue>,
        ) {
            if let Some(FieldValue::Object(map)) = field {
                if let Some(FirstTierField::Option(Some(display))) = map.get("key") {
                    cypher.push_str(&format!("MERGE ({}:JiraUser {{key: \"{}\"}})\nSET ", role.to_lowercase(), display));
                    let mut first_one = true;
                    for (key, value_opt) in map {
                        match value_opt {
                            FirstTierField::Option(Some(v)) => {
                                if !first_one {
                                    cypher.push_str(",");
                                }
                                cypher.push_str(&format!("{}.`{}` = \"{}\" ", role.to_lowercase(), key, v));
                                first_one = false;
                            }
                            FirstTierField::Number(v) => {
                                if !first_one {
                                    cypher.push_str(",");
                                }
                                cypher.push_str(&format!("{}.`{}` = {} ", role.to_lowercase(), key, v));
                                first_one = false;
                            }
                            FirstTierField::Bool(v) => {
                                if !first_one {
                                    cypher.push_str(",");
                                }
                                cypher.push_str(&format!("{}.`{}` = {} ", role.to_lowercase(), key, v));
                                first_one = false;
                            }
                            _ => {
                                // TODO Handle HashMap/List types
                            }
                        }
                    }
                    cypher.push_str(&format!("MERGE (u)-[:{}]->(i)\n", role));
                }
            }
        }

        match state.graph.start_txn().await {
            Ok(mut transaction) => {
                match message {
                    JiraData::Issues(issue_json) => {
                        // Create list of issues
                        let mut issue_cypher = String::new();
                        let mut second_cypher = String::new();

                        let mut first_issue_att = true;
                        let mut params: HashMap<String, String> = HashMap::new();
                        let issue: JiraIssue = serde_json::from_str(&issue_json.json)
                            .expect("Failed to deserialize");
                        println!("Processing issue:{}", &issue.key);
                        params.insert(String::from("issueKey"), issue.key.clone());
                        let mut field_count:u32 = 0;
                        issue_cypher.push_str("MERGE (i:JiraIssue {key: $issueKey})\nSET ");

                        for key in issue.fields.keys() {
                            if let Some(value) = issue.fields.get(&key.clone()) {
                                match value {
                                    FieldValue::OptionValue(Some(v)) => {
                                        if !first_issue_att {
                                            issue_cypher.push_str(",");
                                        }
                                        let new_key = String::from(format!("field{field_count}"));
                                        field_count += 1;
                                        params.insert(new_key.clone(), v.to_string());
                                        issue_cypher.push_str(&format!("i.`{}` = ${new_key}", &key));
                                        first_issue_att = false;
                                    },
                                    FieldValue::Number(v) => {
                                        if !first_issue_att {
                                            issue_cypher.push_str(",");
                                        }
                                        issue_cypher.push_str(&format!("i.`{}` = {}", &key, v));
                                        first_issue_att = false;
                                    },
                                    FieldValue::Bool(v) => {
                                        if !first_issue_att {
                                            issue_cypher.push_str(",");
                                        }
                                        issue_cypher.push_str(&format!("i.`{}` = {}", &key, v));
                                        first_issue_att = false;
                                    },
                                    FieldValue::List(v) => {
                                        if !v.is_empty() {
                                            //println!("Found({:?}) list:{:?}", key, v);
                                            let mut label = key.clone().replace(" ", "_").replace("-", "_").replace("/","").replace('#', "num");

                                            for (i, item) in v.into_iter().enumerate() {
                                                match item {
                                                    NestedListTypes::Option(Some(val)) => {
                                                        let mut sub_key = String::new();
                                                        let new_key = String::from(format!("field{field_count}"));
                                                        field_count += 1;
                                                        params.insert(new_key.clone(), val.to_string());
                                                        second_cypher.push_str(
                                                            &format!(
                                                                "MERGE ( {}{i}:JiraIssue_{label} {{ key: ${new_key} }})\nMERGE (i)-[:HAS_{}]->({}{i})",
                                                                label.to_lowercase(),
                                                                label.to_uppercase(),
                                                                label.to_lowercase(),
                                                            )
                                                        );
                                                    },
                                                    NestedListTypes::Object(the_item) => {
                                                        let mut sub_key = String::new();
                                                        if let Some(FirstTierField::Option(Some(name))) = the_item.get("name") {
                                                            sub_key.push_str(&name);
                                                        } else if let Some(FirstTierField::Option(Some(id))) = the_item.get("id") {
                                                            sub_key.push_str(&id);
                                                        } else {
                                                            sub_key.push_str(&issue.key);
                                                            sub_key.push_str(&i.to_string());
                                                        }
                                                        let new_key = String::from(format!("field{field_count}"));
                                                        field_count += 1;
                                                        params.insert(new_key.clone(), sub_key);
                                                        second_cypher.push_str(
                                                            &format!(
                                                                "MERGE ( {}{i}:JiraIssue_{label} {{ key: ${new_key} }}) SET ",
                                                                label.to_lowercase(),
                                                            )
                                                        );
                                                        let mut first_one = true;
                                                        for attribute in the_item.keys() {
                                                            if let Some(value) = the_item.get(attribute) {
                                                                match value {
                                                                    FirstTierField::Option(Some(val)) => {
                                                                        if !first_one {
                                                                            second_cypher.push_str(",");
                                                                        }
                                                                        let new_sub_key = String::from(format!("field{field_count}"));
                                                                        field_count += 1;
                                                                        params.insert(new_sub_key.clone(), val.to_string());
                                                                        second_cypher.push_str(&format!(
                                                                            "{}{i}.`{}` = ${new_sub_key} ",
                                                                            label.to_lowercase(),
                                                                            &attribute,
                                                                            ));
                                                                        first_one = false;
                                                                    },
                                                                    FirstTierField::Number(val) => {
                                                                        if !first_one {
                                                                            second_cypher.push_str(",");
                                                                        }
                                                                        second_cypher.push_str(&format!(
                                                                            "{}{i}.`{}` = {} ",
                                                                            label.to_lowercase(),
                                                                            &attribute,
                                                                            val));
                                                                        first_one = false;
                                                                    },
                                                                    FirstTierField::Bool(val) => {
                                                                        if !first_one {
                                                                            second_cypher.push_str(",");
                                                                        }
                                                                        second_cypher.push_str(&format!(
                                                                            "{}{i}.`{}` = {} ",
                                                                            label.to_lowercase(),
                                                                            &attribute,
                                                                            val));
                                                                        first_one = false;
                                                                    },
                                                                    _ => ()
                                                                }
                                                            }
                                                        }

                                                        second_cypher.push_str(
                                                            &format!(
                                                                "MERGE (i)-[:HAS_{}]->({}{i})\n",
                                                                label.to_uppercase(),
                                                                label.to_lowercase(),
                                                            ));
                                                    },
                                                    _ => ()
                                                }
                                            }
                                        }
                                    },
                                    FieldValue::Object(v) => {
                                        if key == "Assignee" {
                                            merge_user(&mut second_cypher, "ASSIGNED", issue.fields.get("Assignee"));
                                        } else if key == "Creator" {
                                            merge_user(&mut second_cypher, "CREATED", issue.fields.get("Creator"));
                                        } else if key == "Reporter" {
                                            merge_user(&mut second_cypher, "REPORTED", issue.fields.get("Reporter"));
                                        } else if key == "Issue Type" {

                                            if let Some(FirstTierField::Option(Some(display))) = v.get("name") {
                                                second_cypher.push_str(
                                                    "MERGE (t:JiraIssueType {name: \""
                                                );
                                                second_cypher.push_str(display);
                                                second_cypher.push_str("\"})\n SET ");
                                                let mut first_one = true;
                                                for subkey in v.keys() {
                                                    if let Some(value) = v.get(subkey) {
                                                        match value {
                                                            FirstTierField::Option(Some(val)) => {
                                                                if !first_one {
                                                                    second_cypher.push_str(",");
                                                                }
                                                                let new_key = String::from(format!("field{field_count}"));
                                                                field_count += 1;
                                                                params.insert(new_key.clone(), val.to_string());
                                                                second_cypher.push_str(&format!("t.`{}` = ${new_key} ", &subkey));
                                                                first_one = false;
                                                            },
                                                            FirstTierField::Number(val) => {
                                                                if !first_one {
                                                                    second_cypher.push_str(",");
                                                                }
                                                                second_cypher.push_str(&format!("t.`{}` = {} ", &subkey, val));
                                                                first_one = false;
                                                            },
                                                            FirstTierField::Bool(val) => {
                                                                if !first_one {
                                                                    second_cypher.push_str(",");
                                                                }
                                                                second_cypher.push_str(&format!("t.`{}` = {} ", &subkey, val));
                                                                first_one = false;
                                                            },
                                                            _ => ()
                                                        }
                                                    }
                                                }
                                                second_cypher.push_str("MERGE (i)-[:HAS_TYPE]->(t)\n");
                                            }
                                        } else if key == "Project" {
                                            if let Some(FirstTierField::Option(Some(display))) = v.get("name") {
                                                second_cypher.push_str(
                                                    &format!(
                                                        "MERGE (p:JiraProject {{name: \"{}\"}})\n SET ", display
                                                    )
                                                );

                                                let mut add_comma = false;
                                                for subkey in v.keys() {
                                                    if let Some(value) = v.get(subkey) {
                                                        match value {
                                                            FirstTierField::Object(val) => {
                                                                // Check for object first
                                                                //println!("Skipping sub field key Object: {}", subkey);
                                                            },
                                                            FirstTierField::Option(Some(val)) => {
                                                                if add_comma {
                                                                    second_cypher.push_str(",");
                                                                }
                                                                second_cypher.push_str(&format!("p.`{}` = \"{}\" ", &subkey, val));
                                                                add_comma = true;
                                                            },
                                                            FirstTierField::Number(val) => {
                                                                if add_comma {
                                                                    second_cypher.push_str(",");
                                                                }
                                                                second_cypher.push_str(&format!("p.`{}` = {} ", &subkey, val));
                                                                add_comma = true;
                                                            },
                                                            FirstTierField::Bool(val) => {
                                                                if add_comma {
                                                                    second_cypher.push_str(",");
                                                                }
                                                                second_cypher.push_str(&format!("p.`{}` = {} ", &subkey, val));
                                                                add_comma = true;
                                                            },
                                                            _ => {
                                                                println!("Skipping sub field unknown: {}", &subkey);
                                                            }
                                                        }
                                                    }
                                                }
                                                second_cypher.push_str("MERGE (i)-[:PART_OF]->(p)\n");
                                            }
                                        } else if key == "parent" {
                                            if let Some(FirstTierField::Option(Some(parent_key))) = v.get("name") {
                                                second_cypher.push_str(&format!(
                                                    "MERGE (parent:JiraIssue {{key: \"{}\" }})\n MERGE (i)-[:CHILD_OF]->(parent)\n", parent_key
                                                ));
                                            }
                                        } else {
                                            //println!("Found({:?}) Object:{:?}", key, v);
                                            let mut first_one = true;
                                            let mut sub_key = String::new();
                                            if let Some(FirstTierField::Option(Some(name))) = v.get("name") {
                                                sub_key.push_str(name);
                                            } else if let Some(FirstTierField::Option(Some(id))) = v.get("id") {
                                                sub_key.push_str(id);
                                            } else {
                                                sub_key.push_str(&issue.key);
                                            }
                                            let new_key = String::from(format!("field{field_count}"));
                                            field_count += 1;
                                            params.insert(new_key.clone(), sub_key);
                                            second_cypher.push_str(&format!(
                                                "MERGE ({}: JiraIssue_{} {{key: ${new_key} }})\n SET ",
                                                key.replace(" ", "_").replace("-", "_").to_lowercase(),
                                                key.replace(" ", "_").replace("-", "_")
                                                ));

                                            for attribute in v.keys() {
                                                if let Some(value) = v.get(attribute) {
                                                    match value {
                                                        FirstTierField::Option(Some(val)) => {
                                                            if !first_one {
                                                                second_cypher.push_str(",");
                                                            }
                                                            let new_sub_key = String::from(format!("field{field_count}"));
                                                            field_count += 1;
                                                            params.insert(new_sub_key.clone(), val.to_string());
                                                            second_cypher.push_str(&format!(
                                                                "{}.`{}` = ${new_sub_key} ",
                                                                key.replace(" ", "_").replace("-", "_").to_lowercase(),
                                                                &attribute,
                                                                ));

                                                            first_one = false;
                                                        },
                                                        FirstTierField::Number(val) => {
                                                            if !first_one {
                                                                second_cypher.push_str(",");
                                                            }
                                                            second_cypher.push_str(&format!(
                                                                "{}.`{}` = {} ",
                                                                key.replace(" ", "_").replace("-", "_").to_lowercase(),
                                                                &attribute,
                                                                val));
                                                            first_one = false;
                                                        },
                                                        FirstTierField::Bool(val) => {
                                                            if !first_one {
                                                                second_cypher.push_str(",");
                                                            }
                                                            second_cypher.push_str(&format!(
                                                                "{}.`{}` = {} ",
                                                                key.replace(" ", "_").replace("-", "_").to_lowercase(),
                                                                &attribute,
                                                                val));
                                                            first_one = false;
                                                        },
                                                        _ => ()
                                                    }
                                                }
                                            }

                                            second_cypher.push_str(
                                                &format!(
                                                    "MERGE (i)-[:HAS_{}]->({})\n",
                                                    key.replace(" ", "_").replace("-", "_").to_uppercase(),
                                                    key.replace(" ", "_").replace("-", "_")
                                                ));
                                        }
                                    },
                                    _ => (),
                                }
                            }
                        }

                        // Add logic for changelog
                        let mut changelogs = String::new();
                        let mut counter: u32 = 0;
                        for base_item in issue.changelog.histories {
                            let author = base_item.author;
                            let created = base_item.created;
                            for item in base_item.items {
                                let new_sub_key = String::from(format!("field{field_count}"));
                                field_count += 1;
                                params.insert(new_sub_key.clone(), author.key.clone());
                                changelogs.push_str(
                                    &format!(
                                        "MERGE (cl{}:JiraIssueChangeLog {{baseId: \"{}\", id:{} }}) SET cl{}.author=${new_sub_key}, cl{}.created=\"{}\", cl{}.field=\"{}\", cl{}.fieldtype=\"{}\" ",
                                        counter.to_string(),
                                        base_item.id,
                                        counter.to_string(),
                                        counter.to_string(),
                                        counter.to_string(),
                                        created,
                                        counter.to_string(),
                                        item.field,
                                        counter.to_string(),
                                        item.fieldtype
                                    )
                                );

                                if let Some(option) = item.from {
                                    let new_field_key = String::from(format!("field{field_count}"));
                                    field_count += 1;
                                    params.insert(new_field_key.clone(), option);
                                    changelogs.push_str(
                                        &format!(
                                            ", cl{}.from=${new_field_key}",
                                            counter.to_string()
                                        )
                                    );
                                }
                                if let Some(option) = item.fromString {
                                    let new_field_key = String::from(format!("field{field_count}"));
                                    field_count += 1;
                                    params.insert(new_field_key.clone(), option);
                                    changelogs.push_str(
                                        &format!(
                                            ", cl{}.fromString=${new_field_key}",
                                            counter.to_string()
                                        )
                                    );
                                }
                                if let Some(option) = item.to {
                                    let new_field_key = String::from(format!("field{field_count}"));
                                    field_count += 1;
                                    params.insert(new_field_key.clone(), option);
                                    changelogs.push_str(
                                        &format!(
                                            ", cl{}.to=${new_field_key}",
                                            counter.to_string()
                                        )
                                    );
                                }
                                if let Some(option) = item.toString {
                                    let new_field_key = String::from(format!("field{field_count}"));
                                    field_count += 1;
                                    params.insert(new_field_key.clone(), option);
                                    changelogs.push_str(
                                        &format!(
                                            ", cl{}.toString=${new_field_key}",
                                            counter.to_string()
                                        )
                                    );
                                }
                                changelogs.push_str("\n");
                                changelogs.push_str(&format!(
                                    "MERGE (i)-[:Transitioned]->(cl{})\n",
                                    counter.to_string(),
                                    ));
                                counter += 1;
                            }
                        }

                        issue_cypher.push('\n');
                        issue_cypher.push_str(&second_cypher);
                        issue_cypher.push_str(&changelogs);

                        if let Err(_e) = transaction.run(Query::new(issue_cypher.clone()).params(params)).await {
                            println!("Cypher is:{}", issue_cypher);
                            println!("Error:{:?}", _e);
                            myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                        }

                        if let Err(_e) = transaction.commit().await {
                            println!("Error Commit:{:?}", _e);
                            myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                        }
                    },
                    _ => (),
                }
            }
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}"))),
        }
        Ok(())
    }
}
