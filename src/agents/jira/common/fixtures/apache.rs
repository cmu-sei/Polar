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

// Real answers from Apache's Jira, a public Data Center (https://issues.apache.org/jira, version 10.3.26,
// deploymentType "Server"), read anonymously once each with curl on 2026-11-01:
//
//   GET /rest/api/2/serverInfo
//   GET /rest/api/2/search?jql=project = COMDEV ORDER BY created ASC&maxResults=3&expand=changelog
//   GET /rest/api/2/field
//
// Trimmed by hand, nothing else changed: the search keeps COMDEV-3, -4 and -5 whole except that custom fields
// whose value is null are left out (but for customfield_12312823, Apache's custom field named "Project", the
// display name of the system field `project` too, and customfield_12314132, "Issue & fix info", a name the graph
// refuses), and the 2 KiB plugin dump in customfield_12314020 ("Development") is cut to 120 characters; the field
// list keeps the fields those issues carry. What Apache's site sends is what any Data Center that hides email
// addresses sends: changelog authors with name, key, displayName, avatarUrls, active, timeZone and self, and no
// emailAddress.
//
// Included (`include!`) by the tests of jira-observer and jira-processor; not part of either build.

/// `GET /rest/api/2/serverInfo`.
pub const SERVER_INFO: &str = r##"{
 "baseUrl": "https://issues.apache.org/jira",
 "version": "10.3.26",
 "versionNumbers": [
  10,
  3,
  26
 ],
 "deploymentType": "Server",
 "buildNumber": 10030026,
 "buildDate": "2026-10-02T00:00:00.000+0000",
 "databaseBuildNumber": 10030026,
 "scmInfo": "1ecd6b128104dee61aadbfb736809c20260e9b3f",
 "serverTitle": "ASF Jira"
}"##;

/// `GET /rest/api/2/field`, the fields the issues below carry.
pub const FIELDS: &str = r##"[
 {
  "id": "resolution",
  "name": "Resolution",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "resolution"
  ],
  "schema": {
   "type": "resolution",
   "system": "resolution"
  }
 },
 {
  "id": "lastViewed",
  "name": "Last Viewed",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "lastViewed"
  ],
  "schema": {
   "type": "datetime",
   "system": "lastViewed"
  }
 },
 {
  "id": "labels",
  "name": "Labels",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "labels"
  ],
  "schema": {
   "type": "array",
   "items": "string",
   "system": "labels"
  }
 },
 {
  "id": "customfield_12313422",
  "name": "Enable Automatic Patch Review",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12313422]",
   "Enable Automatic Patch Review"
  ],
  "schema": {
   "type": "string",
   "custom": "com.atlassian.jirafisheyeplugin:jobcheckbox",
   "customId": 12313422
  }
 },
 {
  "id": "aggregatetimeoriginalestimate",
  "name": "Σ Original Estimate",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [],
  "schema": {
   "type": "number",
   "system": "aggregatetimeoriginalestimate"
  }
 },
 {
  "id": "issuelinks",
  "name": "Linked Issues",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [],
  "schema": {
   "type": "array",
   "items": "issuelinks",
   "system": "issuelinks"
  }
 },
 {
  "id": "assignee",
  "name": "Assignee",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "assignee"
  ],
  "schema": {
   "type": "user",
   "system": "assignee"
  }
 },
 {
  "id": "components",
  "name": "Component/s",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "component"
  ],
  "schema": {
   "type": "array",
   "items": "component",
   "system": "components"
  }
 },
 {
  "id": "customfield_12310920",
  "name": "Rank (Obsolete)",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12310920]",
   "Rank (Obsolete)"
  ],
  "schema": {
   "type": "any",
   "custom": "com.pyxis.greenhopper.jira:gh-global-rank",
   "customId": 12310920
  }
 },
 {
  "id": "subtasks",
  "name": "Sub-Tasks",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "subtasks"
  ],
  "schema": {
   "type": "array",
   "items": "issuelinks",
   "system": "subtasks"
  }
 },
 {
  "id": "reporter",
  "name": "Reporter",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "reporter"
  ],
  "schema": {
   "type": "user",
   "system": "reporter"
  }
 },
 {
  "id": "progress",
  "name": "Progress",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "progress"
  ],
  "schema": {
   "type": "progress",
   "system": "progress"
  }
 },
 {
  "id": "votes",
  "name": "Votes",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "votes"
  ],
  "schema": {
   "type": "votes",
   "system": "votes"
  }
 },
 {
  "id": "issuetype",
  "name": "Issue Type",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "issuetype",
   "type"
  ],
  "schema": {
   "type": "issuetype",
   "system": "issuetype"
  }
 },
 {
  "id": "project",
  "name": "Project",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "project"
  ],
  "schema": {
   "type": "project",
   "system": "project"
  }
 },
 {
  "id": "resolutiondate",
  "name": "Resolved",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "resolutiondate",
   "resolved"
  ],
  "schema": {
   "type": "datetime",
   "system": "resolutiondate"
  }
 },
 {
  "id": "watches",
  "name": "Watchers",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "watchers"
  ],
  "schema": {
   "type": "watches",
   "system": "watches"
  }
 },
 {
  "id": "updated",
  "name": "Updated",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "updated",
   "updatedDate"
  ],
  "schema": {
   "type": "datetime",
   "system": "updated"
  }
 },
 {
  "id": "timeoriginalestimate",
  "name": "Original Estimate",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "originalEstimate",
   "timeoriginalestimate"
  ],
  "schema": {
   "type": "number",
   "system": "timeoriginalestimate"
  }
 },
 {
  "id": "description",
  "name": "Description",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "description"
  ],
  "schema": {
   "type": "string",
   "system": "description"
  }
 },
 {
  "id": "summary",
  "name": "Summary",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "summary"
  ],
  "schema": {
   "type": "string",
   "system": "summary"
  }
 },
 {
  "id": "environment",
  "name": "Environment",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "environment"
  ],
  "schema": {
   "type": "string",
   "system": "environment"
  }
 },
 {
  "id": "duedate",
  "name": "Due Date",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "due",
   "duedate"
  ],
  "schema": {
   "type": "date",
   "system": "duedate"
  }
 },
 {
  "id": "customfield_12311820",
  "name": "Rank",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12311820]",
   "Rank"
  ],
  "schema": {
   "type": "any",
   "custom": "com.pyxis.greenhopper.jira:gh-lexo-rank",
   "customId": 12311820
  }
 },
 {
  "id": "fixVersions",
  "name": "Fix Version/s",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "fixVersion"
  ],
  "schema": {
   "type": "array",
   "items": "version",
   "system": "fixVersions"
  }
 },
 {
  "id": "customfield_12310420",
  "name": "Global Rank",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12310420]",
   "Global Rank"
  ],
  "schema": {
   "type": "any",
   "custom": "com.pyxis.greenhopper.jira:gh-global-rank",
   "customId": 12310420
  }
 },
 {
  "id": "priority",
  "name": "Priority",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "priority"
  ],
  "schema": {
   "type": "priority",
   "system": "priority"
  }
 },
 {
  "id": "customfield_12310310",
  "name": "Attachment count",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "Attachment count",
   "cf[12310310]"
  ],
  "schema": {
   "type": "any",
   "custom": "com.atlassian.jira.toolkit:attachments",
   "customId": 12310310
  }
 },
 {
  "id": "timeestimate",
  "name": "Remaining Estimate",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "remainingEstimate",
   "timeestimate"
  ],
  "schema": {
   "type": "number",
   "system": "timeestimate"
  }
 },
 {
  "id": "versions",
  "name": "Affects Version/s",
  "custom": false,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "affectedVersion"
  ],
  "schema": {
   "type": "array",
   "items": "version",
   "system": "versions"
  }
 },
 {
  "id": "status",
  "name": "Status",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "status"
  ],
  "schema": {
   "type": "status",
   "system": "status"
  }
 },
 {
  "id": "aggregatetimeestimate",
  "name": "Σ Remaining Estimate",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [],
  "schema": {
   "type": "number",
   "system": "aggregatetimeestimate"
  }
 },
 {
  "id": "customfield_12312823",
  "name": "Project",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12312823]"
  ],
  "schema": {
   "type": "option",
   "custom": "com.atlassian.jira.plugin.system.customfieldtypes:select",
   "customId": 12312823
  }
 },
 {
  "id": "creator",
  "name": "Creator",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "creator"
  ],
  "schema": {
   "type": "user",
   "system": "creator"
  }
 },
 {
  "id": "aggregateprogress",
  "name": "Σ Progress",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [],
  "schema": {
   "type": "progress",
   "system": "aggregateprogress"
  }
 },
 {
  "id": "timespent",
  "name": "Time Spent",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [
   "timespent"
  ],
  "schema": {
   "type": "number",
   "system": "timespent"
  }
 },
 {
  "id": "customfield_12314020",
  "name": "Development",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12314020]",
   "Development"
  ],
  "schema": {
   "type": "any",
   "custom": "com.atlassian.jira.plugins.jira-development-integration-plugin:devsummary",
   "customId": 12314020
  }
 },
 {
  "id": "aggregatetimespent",
  "name": "Σ Time Spent",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": false,
  "clauseNames": [],
  "schema": {
   "type": "number",
   "system": "aggregatetimespent"
  }
 },
 {
  "id": "customfield_12312521",
  "name": "Last public comment date",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12312521]",
   "Last public comment date"
  ],
  "schema": {
   "type": "any",
   "custom": "com.atlassian.jira.toolkit:LastCommentDate",
   "customId": 12312521
  }
 },
 {
  "id": "workratio",
  "name": "Work Ratio",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "workratio"
  ],
  "schema": {
   "type": "number",
   "system": "workratio"
  }
 },
 {
  "id": "created",
  "name": "Created",
  "custom": false,
  "orderable": false,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "created",
   "createdDate"
  ],
  "schema": {
   "type": "datetime",
   "system": "created"
  }
 },
 {
  "id": "customfield_12314132",
  "name": "Issue & fix info",
  "custom": true,
  "orderable": true,
  "navigable": true,
  "searchable": true,
  "clauseNames": [
   "cf[12314132]",
   "Issue & fix info"
  ],
  "schema": {
   "type": "array",
   "items": "option",
   "custom": "com.atlassian.jira.plugin.system.customfieldtypes:multicheckboxes",
   "customId": 12314132
  }
 }
]"##;

/// `GET /rest/api/2/search`, its first page of three issues, each with its changelog.
pub const SEARCH: &str = r##"{
 "expand": "schema,names",
 "startAt": 0,
 "maxResults": 3,
 "total": 300,
 "issues": [
  {
   "expand": "operations,versionedRepresentations,editmeta,changelog,renderedFields",
   "id": "12441915",
   "self": "https://issues.apache.org/jira/rest/api/2/issue/12441915",
   "key": "COMDEV-3",
   "fields": {
    "fixVersions": [],
    "resolution": {
     "self": "https://issues.apache.org/jira/rest/api/2/resolution/6",
     "id": "6",
     "description": "The problem isn't valid and it can't be fixed.",
     "name": "Invalid"
    },
    "customfield_12310420": "110423",
    "lastViewed": null,
    "priority": {
     "self": "https://issues.apache.org/jira/rest/api/2/priority/3",
     "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/major.svg",
     "name": "Major",
     "id": "3"
    },
    "labels": [],
    "customfield_12313422": "false",
    "customfield_12310310": "0.0",
    "timeestimate": 86400,
    "aggregatetimeoriginalestimate": 86400,
    "versions": [],
    "issuelinks": [],
    "assignee": null,
    "status": {
     "self": "https://issues.apache.org/jira/rest/api/2/status/6",
     "description": "The issue is considered finished, the resolution is correct. Issues which are not closed can be reopened.",
     "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/closed.png",
     "name": "Closed",
     "id": "6",
     "statusCategory": {
      "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
      "id": 3,
      "key": "done",
      "colorName": "success",
      "name": "Done"
     }
    },
    "components": [],
    "aggregatetimeestimate": 86400,
    "customfield_12310920": "267441",
    "customfield_12312823": null,
    "creator": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=btsulliv",
     "name": "btsulliv",
     "key": "btsulliv",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Blake Sullivan",
     "active": true,
     "timeZone": "America/Los_Angeles"
    },
    "subtasks": [],
    "reporter": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=btsulliv",
     "name": "btsulliv",
     "key": "btsulliv",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Blake Sullivan",
     "active": true,
     "timeZone": "America/Los_Angeles"
    },
    "aggregateprogress": {
     "progress": 0,
     "total": 86400,
     "percent": 0
    },
    "progress": {
     "progress": 0,
     "total": 86400,
     "percent": 0
    },
    "votes": {
     "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-3/votes",
     "votes": 0,
     "hasVoted": false
    },
    "issuetype": {
     "self": "https://issues.apache.org/jira/rest/api/2/issuetype/1",
     "id": "1",
     "description": "A problem which impairs or prevents the functions of the product.",
     "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21133&avatarType=issuetype",
     "name": "Bug",
     "subtask": false,
     "avatarId": 21133
    },
    "timespent": null,
    "customfield_12314020": "{summaryBean=com.atlassian.jira.plugin.devstatus.rest.SummaryBean@1deec5f1[summary={pullrequest=com.atlassian.jira.plugi...",
    "project": {
     "self": "https://issues.apache.org/jira/rest/api/2/project/12311010",
     "id": "12311010",
     "key": "COMDEV",
     "name": "Community Development",
     "projectTypeKey": "software",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/projectavatar?pid=12311010&avatarId=10011",
      "24x24": "https://issues.apache.org/jira/secure/projectavatar?size=small&pid=12311010&avatarId=10011",
      "16x16": "https://issues.apache.org/jira/secure/projectavatar?size=xsmall&pid=12311010&avatarId=10011",
      "32x32": "https://issues.apache.org/jira/secure/projectavatar?size=medium&pid=12311010&avatarId=10011"
     },
     "projectCategory": {
      "self": "https://issues.apache.org/jira/rest/api/2/projectCategory/10130",
      "id": "10130",
      "description": "General ASF-wide projects.",
      "name": "Apache Software Foundation"
     }
    },
    "aggregatetimespent": null,
    "customfield_12312521": "Mon Nov 30 18:03:01 UTC 2009",
    "resolutiondate": "2009-11-30T18:03:01.000+0000",
    "workratio": 0,
    "watches": {
     "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-3/watchers",
     "watchCount": 0,
     "isWatching": false
    },
    "created": "2009-11-30T01:57:55.000+0000",
    "updated": "2009-11-30T18:03:01.000+0000",
    "timeoriginalestimate": 86400,
    "description": "The HTML specification disallows certain code points from appearing in HTML files (XML has essentially the same list, minus the high ISO characters) as specified in http://www.w3.org/TR/REC-html40/sgml/sgmldecl.html.  The Trinidad HtmlEscapes utilities allow the low control characters and the high characters that are technically outside of Unicode to be output.  This causes problems if the content is validated.\n\nThe fix is to use numeric character references such as &#1; rather than outputting code point 1 directly.  In addition, Internet Explorer has a bug where &#0; is output as \"&#0;\" so it is preferable to suppress this character rather then outputting it.\n\n",
    "summary": "HTML Escapes writes out characters illegal in HTML",
    "environment": "generic",
    "duedate": null,
    "customfield_12314132": null,
    "customfield_12311820": "0|i1a60n:"
   },
   "renderedFields": null,
   "changelog": {
    "startAt": 0,
    "maxResults": 1,
    "total": 1,
    "histories": [
     {
      "id": "12791647",
      "author": {
       "self": "https://issues.apache.org/jira/rest/api/2/user?username=btsulliv",
       "name": "btsulliv",
       "key": "btsulliv",
       "avatarUrls": {
        "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
        "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
        "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
        "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
       },
       "displayName": "Blake Sullivan",
       "active": true,
       "timeZone": "America/Los_Angeles"
      },
      "created": "2009-11-30T18:03:01.771+0000",
      "items": [
       {
        "field": "resolution",
        "fieldtype": "jira",
        "from": null,
        "fromString": null,
        "to": "6",
        "toString": "Invalid"
       },
       {
        "field": "status",
        "fieldtype": "jira",
        "from": "1",
        "fromString": "Open",
        "to": "6",
        "toString": "Closed"
       }
      ]
     }
    ]
   }
  },
  {
   "expand": "operations,versionedRepresentations,editmeta,changelog,renderedFields",
   "id": "12442326",
   "self": "https://issues.apache.org/jira/rest/api/2/issue/12442326",
   "key": "COMDEV-4",
   "fields": {
    "fixVersions": [],
    "resolution": {
     "self": "https://issues.apache.org/jira/rest/api/2/resolution/1",
     "id": "1",
     "description": "A fix for this issue is checked into the tree and tested.",
     "name": "Fixed"
    },
    "customfield_12310420": "110424",
    "lastViewed": null,
    "priority": {
     "self": "https://issues.apache.org/jira/rest/api/2/priority/3",
     "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/major.svg",
     "name": "Major",
     "id": "3"
    },
    "labels": [],
    "customfield_12313422": "false",
    "customfield_12310310": "0.0",
    "timeestimate": null,
    "aggregatetimeoriginalestimate": null,
    "versions": [],
    "issuelinks": [
     {
      "id": "12328123",
      "self": "https://issues.apache.org/jira/rest/api/2/issueLink/12328123",
      "outwardIssue": {
       "id": "21720",
       "key": "XERCESJ-977",
       "self": "https://issues.apache.org/jira/rest/api/2/issue/21720",
       "fields": {
        "summary": "Null pointer exception during DOM parsing",
        "status": {
         "self": "https://issues.apache.org/jira/rest/api/2/status/5",
         "description": "A resolution has been taken, and it is awaiting verification by reporter. From here issues are either reopened, or are closed.",
         "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/resolved.png",
         "name": "Resolved",
         "id": "5",
         "statusCategory": {
          "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
          "id": 3,
          "key": "done",
          "colorName": "success",
          "name": "Done"
         }
        },
        "priority": {
         "self": "https://issues.apache.org/jira/rest/api/2/priority/3",
         "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/major.svg",
         "name": "Major",
         "id": "3"
        },
        "issuetype": {
         "self": "https://issues.apache.org/jira/rest/api/2/issuetype/1",
         "id": "1",
         "description": "A problem which impairs or prevents the functions of the product.",
         "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21133&avatarType=issuetype",
         "name": "Bug",
         "subtask": false,
         "avatarId": 21133
        }
       }
      },
      "type": {
       "id": "10030",
       "name": "Reference",
       "inward": "is related to",
       "outward": "relates to",
       "self": "https://issues.apache.org/jira/rest/api/2/issueLinkType/10030"
      }
     }
    ],
    "assignee": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=rgardler",
     "name": "rgardler",
     "key": "rgardler",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Ross Gardler",
     "active": true,
     "timeZone": "America/Los_Angeles"
    },
    "status": {
     "self": "https://issues.apache.org/jira/rest/api/2/status/6",
     "description": "The issue is considered finished, the resolution is correct. Issues which are not closed can be reopened.",
     "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/closed.png",
     "name": "Closed",
     "id": "6",
     "statusCategory": {
      "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
      "id": 3,
      "key": "done",
      "colorName": "success",
      "name": "Done"
     }
    },
    "components": [],
    "aggregatetimeestimate": null,
    "customfield_12310920": "267440",
    "customfield_12312823": null,
    "creator": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=sri_kumar_4u",
     "name": "sri_kumar_4u",
     "key": "sri_kumar_4u",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "sri kumar ",
     "active": false,
     "timeZone": "Etc/UTC"
    },
    "subtasks": [],
    "reporter": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=sri_kumar_4u",
     "name": "sri_kumar_4u",
     "key": "sri_kumar_4u",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "sri kumar ",
     "active": false,
     "timeZone": "Etc/UTC"
    },
    "aggregateprogress": {
     "progress": 0,
     "total": 0
    },
    "progress": {
     "progress": 0,
     "total": 0
    },
    "votes": {
     "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-4/votes",
     "votes": 0,
     "hasVoted": false
    },
    "issuetype": {
     "self": "https://issues.apache.org/jira/rest/api/2/issuetype/11",
     "id": "11",
     "description": "A formal question. Initially added for the Legal JIRA.",
     "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21130&avatarType=issuetype",
     "name": "Question",
     "subtask": false,
     "avatarId": 21130
    },
    "timespent": null,
    "customfield_12314020": "{summaryBean=com.atlassian.jira.plugin.devstatus.rest.SummaryBean@41b56f95[summary={pullrequest=com.atlassian.jira.plugi...",
    "project": {
     "self": "https://issues.apache.org/jira/rest/api/2/project/12311010",
     "id": "12311010",
     "key": "COMDEV",
     "name": "Community Development",
     "projectTypeKey": "software",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/projectavatar?pid=12311010&avatarId=10011",
      "24x24": "https://issues.apache.org/jira/secure/projectavatar?size=small&pid=12311010&avatarId=10011",
      "16x16": "https://issues.apache.org/jira/secure/projectavatar?size=xsmall&pid=12311010&avatarId=10011",
      "32x32": "https://issues.apache.org/jira/secure/projectavatar?size=medium&pid=12311010&avatarId=10011"
     },
     "projectCategory": {
      "self": "https://issues.apache.org/jira/rest/api/2/projectCategory/10130",
      "id": "10130",
      "description": "General ASF-wide projects.",
      "name": "Apache Software Foundation"
     }
    },
    "aggregatetimespent": null,
    "customfield_12312521": "Fri Dec 04 12:22:31 UTC 2009",
    "resolutiondate": "2009-12-04T12:22:31.000+0000",
    "workratio": -1,
    "watches": {
     "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-4/watchers",
     "watchCount": 0,
     "isWatching": false
    },
    "created": "2009-12-03T09:59:30.000+0000",
    "updated": "2009-12-04T12:22:31.000+0000",
    "timeoriginalestimate": null,
    "description": "I was trying to parse a XML Document using DOM Parser.\n\nI got null pointer exception while executing doc = builder.parse(xmlDataFile);\n\nThere were few entities in the XML data file. On removing a particular entity, i was able to parse the file successfully\n\nThe entity was some thing like this\n\n<!ENTITY SAMPLE.TIF SYSTEM \"SAMPLE.TIF\" NDATA TIF>\n\nWhat could be the reason?\n\nEDIT:\n\nHere is the code:\n\n        DocumentBuilderFactory aFactory = DocumentBuilderFactory.newInstance();\n        aFactory.setValidating(false);\n        aFactory.setFeature(\"http://xml.org/sax/features/namespaces\", false);\n        aFactory.setFeature(\"http://apache.org/xml/features/validation/schema\", false);\n        aFactory.setIgnoringComments(true);\n        builder = aFactory.newDocumentBuilder();\n        doc = builder.parse(xmlDataFile);\nThis is the trace:\n\n at com.sun.org.apache.xerces.internal.dom.DeferredDocumentImpl.setChunkIndex(DeferredDocumentImpl.java:1944)\n    at com.sun.org.apache.xerces.internal.dom.DeferredDocumentImpl.appendChild(DeferredDocumentImpl.java:644)\n    at com.sun.org.apache.xerces.internal.parsers.AbstractDOMParser.characters(AbstractDOMParser.java:1191)\n    at com.sun.org.apache.xerces.internal.impl.dtd.XMLDTDValidator.characters(XMLDTDValidator.java:862)\n    at com.sun.org.apache.xerces.internal.impl.XMLDocumentFragmentScannerImpl.scanDocument(XMLDocumentFragmentScannerImpl.java:463)\n    at com.sun.org.apache.xerces.internal.parsers.XML11Configuration.parse(XML11Configuration.java:807)\n    at com.sun.org.apache.xerces.internal.parsers.XML11Configuration.parse(XML11Configuration.java:737)\n    at com.sun.org.apache.xerces.internal.parsers.XMLParser.parse(XMLParser.java:107)\n    at com.sun.org.apache.xerces.internal.parsers.DOMParser.parse(DOMParser.java:225)\n    at com.sun.org.apache.xerces.internal.jaxp.DocumentBuilderImpl.parse(DocumentBuilderImpl.java:283)\n    at javax.xml.parsers.DocumentBuilder.parse(DocumentBuilder.java:180)\n    at XMLParser.Parse(XMLParser.java:89)\n    at Main.main(Main.java:116)\n\n\n89 line number points to -> doc = builder.parse(xmlDataFile);",
    "summary": "External entity in XML causing null pointer exception during DocumentBuilder.parse",
    "environment": null,
    "duedate": null,
    "customfield_12314132": null,
    "customfield_12311820": "0|i1a60f:"
   },
   "renderedFields": null,
   "changelog": {
    "startAt": 0,
    "maxResults": 2,
    "total": 2,
    "histories": [
     {
      "id": "12793179",
      "author": {
       "self": "https://issues.apache.org/jira/rest/api/2/user?username=sri_kumar_4u",
       "name": "sri_kumar_4u",
       "key": "sri_kumar_4u",
       "avatarUrls": {
        "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
        "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
        "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
        "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
       },
       "displayName": "sri kumar ",
       "active": false,
       "timeZone": "Etc/UTC"
      },
      "created": "2009-12-04T11:38:05.962+0000",
      "items": [
       {
        "field": "Link",
        "fieldtype": "jira",
        "from": null,
        "fromString": null,
        "to": "XERCESJ-977",
        "toString": "This issue relates to XERCESJ-977"
       }
      ]
     },
     {
      "id": "12793186",
      "author": {
       "self": "https://issues.apache.org/jira/rest/api/2/user?username=rgardler",
       "name": "rgardler",
       "key": "rgardler",
       "avatarUrls": {
        "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
        "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
        "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
        "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
       },
       "displayName": "Ross Gardler",
       "active": true,
       "timeZone": "America/Los_Angeles"
      },
      "created": "2009-12-04T12:22:31.213+0000",
      "items": [
       {
        "field": "assignee",
        "fieldtype": "jira",
        "from": null,
        "fromString": null,
        "to": "rgardler",
        "toString": "Ross Gardler"
       },
       {
        "field": "resolution",
        "fieldtype": "jira",
        "from": null,
        "fromString": null,
        "to": "1",
        "toString": "Fixed"
       },
       {
        "field": "status",
        "fieldtype": "jira",
        "from": "1",
        "fromString": "Open",
        "to": "6",
        "toString": "Closed"
       }
      ]
     }
    ]
   }
  },
  {
   "expand": "operations,versionedRepresentations,editmeta,changelog,renderedFields",
   "id": "12442579",
   "self": "https://issues.apache.org/jira/rest/api/2/issue/12442579",
   "key": "COMDEV-5",
   "fields": {
    "fixVersions": [],
    "resolution": {
     "self": "https://issues.apache.org/jira/rest/api/2/resolution/6",
     "id": "6",
     "description": "The problem isn't valid and it can't be fixed.",
     "name": "Invalid"
    },
    "customfield_12310420": "110425",
    "lastViewed": null,
    "priority": {
     "self": "https://issues.apache.org/jira/rest/api/2/priority/4",
     "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/minor.svg",
     "name": "Minor",
     "id": "4"
    },
    "labels": [],
    "customfield_12313422": "false",
    "customfield_12310310": "0.0",
    "timeestimate": null,
    "aggregatetimeoriginalestimate": null,
    "versions": [],
    "issuelinks": [],
    "assignee": null,
    "status": {
     "self": "https://issues.apache.org/jira/rest/api/2/status/6",
     "description": "The issue is considered finished, the resolution is correct. Issues which are not closed can be reopened.",
     "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/closed.png",
     "name": "Closed",
     "id": "6",
     "statusCategory": {
      "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
      "id": 3,
      "key": "done",
      "colorName": "success",
      "name": "Done"
     }
    },
    "components": [],
    "aggregatetimeestimate": null,
    "customfield_12310920": "267439",
    "customfield_12312823": null,
    "creator": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=tanjiaqi",
     "name": "tanjiaqi",
     "key": "tanjiaqi",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Jiaqi Tan",
     "active": true,
     "timeZone": "Etc/UTC"
    },
    "subtasks": [],
    "reporter": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=tanjiaqi",
     "name": "tanjiaqi",
     "key": "tanjiaqi",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Jiaqi Tan",
     "active": true,
     "timeZone": "Etc/UTC"
    },
    "aggregateprogress": {
     "progress": 0,
     "total": 0
    },
    "progress": {
     "progress": 0,
     "total": 0
    },
    "votes": {
     "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-5/votes",
     "votes": 0,
     "hasVoted": false
    },
    "issuetype": {
     "self": "https://issues.apache.org/jira/rest/api/2/issuetype/4",
     "id": "4",
     "description": "An improvement or enhancement to an existing feature or task.",
     "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21140&avatarType=issuetype",
     "name": "Improvement",
     "subtask": false,
     "avatarId": 21140
    },
    "timespent": null,
    "customfield_12314020": "{summaryBean=com.atlassian.jira.plugin.devstatus.rest.SummaryBean@74e8c92f[summary={pullrequest=com.atlassian.jira.plugi...",
    "project": {
     "self": "https://issues.apache.org/jira/rest/api/2/project/12311010",
     "id": "12311010",
     "key": "COMDEV",
     "name": "Community Development",
     "projectTypeKey": "software",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/projectavatar?pid=12311010&avatarId=10011",
      "24x24": "https://issues.apache.org/jira/secure/projectavatar?size=small&pid=12311010&avatarId=10011",
      "16x16": "https://issues.apache.org/jira/secure/projectavatar?size=xsmall&pid=12311010&avatarId=10011",
      "32x32": "https://issues.apache.org/jira/secure/projectavatar?size=medium&pid=12311010&avatarId=10011"
     },
     "projectCategory": {
      "self": "https://issues.apache.org/jira/rest/api/2/projectCategory/10130",
      "id": "10130",
      "description": "General ASF-wide projects.",
      "name": "Apache Software Foundation"
     }
    },
    "aggregatetimespent": null,
    "customfield_12312521": "Sun Dec 06 17:15:56 UTC 2009",
    "resolutiondate": "2009-12-06T17:15:56.000+0000",
    "workratio": -1,
    "watches": {
     "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-5/watchers",
     "watchCount": 0,
     "isWatching": false
    },
    "created": "2009-12-06T17:15:27.000+0000",
    "updated": "2009-12-06T17:15:56.000+0000",
    "timeoriginalestimate": null,
    "description": "Driver for running FSMBuilder for the job history logs, DataNode ClientTrace messages, and TaskTracker ClientTrace messages. Takes input locations of log files and output location to write to. Calls the FSMBuilder class, which then returns calling conventions if insufficient parameters are specified.",
    "summary": "FSMBuilder driver script",
    "environment": null,
    "duedate": null,
    "customfield_12314132": null,
    "customfield_12311820": "0|i1a607:"
   },
   "renderedFields": null,
   "changelog": {
    "startAt": 0,
    "maxResults": 1,
    "total": 1,
    "histories": [
     {
      "id": "12793875",
      "author": {
       "self": "https://issues.apache.org/jira/rest/api/2/user?username=tanjiaqi",
       "name": "tanjiaqi",
       "key": "tanjiaqi",
       "avatarUrls": {
        "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
        "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
        "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
        "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
       },
       "displayName": "Jiaqi Tan",
       "active": true,
       "timeZone": "Etc/UTC"
      },
      "created": "2009-12-06T17:15:56.934+0000",
      "items": [
       {
        "field": "resolution",
        "fieldtype": "jira",
        "from": null,
        "fromString": null,
        "to": "6",
        "toString": "Invalid"
       },
       {
        "field": "status",
        "fieldtype": "jira",
        "from": "1",
        "fromString": "Open",
        "to": "6",
        "toString": "Closed"
       }
      ]
     }
    ]
   }
  }
 ],
 "warningMessages": null,
 "names": null,
 "schema": null
}"##;

/// What jira-observer publishes for each issue of [`SEARCH`] (`SiteFields::prepare` with [`FIELDS`]):
/// the system fields beside the fields by id, the others by display name, the last update beside them.
/// Pinned by jira-observer's tests, and what jira-processor's tests write.
pub const PUBLISHED: &[&str] = &[
    r##"{
 "expand": "operations,versionedRepresentations,editmeta,changelog,renderedFields",
 "id": "12441915",
 "self": "https://issues.apache.org/jira/rest/api/2/issue/12441915",
 "key": "COMDEV-3",
 "fields": {
  "Attachment count": "0.0",
  "Created": "2009-11-30T01:57:55.000+0000",
  "Σ Time Spent": null,
  "Description": "The HTML specification disallows certain code points from appearing in HTML files (XML has essentially the same list, minus the high ISO characters) as specified in http://www.w3.org/TR/REC-html40/sgml/sgmldecl.html.  The Trinidad HtmlEscapes utilities allow the low control characters and the high characters that are technically outside of Unicode to be output.  This causes problems if the content is validated.\n\nThe fix is to use numeric character references such as &#1; rather than outputting code point 1 directly.  In addition, Internet Explorer has a bug where &#0; is output as \"&#0;\" so it is preferable to suppress this character rather then outputting it.\n\n",
  "Development": "{summaryBean=com.atlassian.jira.plugin.devstatus.rest.SummaryBean@1deec5f1[summary={pullrequest=com.atlassian.jira.plugi...",
  "Original Estimate": 86400,
  "Rank": "0|i1a60n:",
  "Remaining Estimate": 86400,
  "Votes": {
   "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-3/votes",
   "votes": 0,
   "hasVoted": false
  },
  "Watchers": {
   "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-3/watchers",
   "watchCount": 0,
   "isWatching": false
  },
  "Component/s": [],
  "Resolution": {
   "self": "https://issues.apache.org/jira/rest/api/2/resolution/6",
   "id": "6",
   "description": "The problem isn't valid and it can't be fixed.",
   "name": "Invalid"
  },
  "Summary": "HTML Escapes writes out characters illegal in HTML",
  "Resolved": "2009-11-30T18:03:01.000+0000",
  "Priority": {
   "self": "https://issues.apache.org/jira/rest/api/2/priority/3",
   "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/major.svg",
   "name": "Major",
   "id": "3"
  },
  "Global Rank": "110423",
  "Linked Issues": [],
  "Time Spent": null,
  "Updated": "2009-11-30T18:03:01.000+0000",
  "Σ Original Estimate": 86400,
  "Status": {
   "self": "https://issues.apache.org/jira/rest/api/2/status/6",
   "description": "The issue is considered finished, the resolution is correct. Issues which are not closed can be reopened.",
   "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/closed.png",
   "name": "Closed",
   "id": "6",
   "statusCategory": {
    "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
    "id": 3,
    "key": "done",
    "colorName": "success",
    "name": "Done"
   }
  },
  "Sub-Tasks": [],
  "Σ Progress": {
   "progress": 0,
   "total": 86400,
   "percent": 0
  },
  "Last Viewed": null,
  "Project": null,
  "Progress": {
   "progress": 0,
   "total": 86400,
   "percent": 0
  },
  "Environment": "generic",
  "Labels": [],
  "Last public comment date": "Mon Nov 30 18:03:01 UTC 2009",
  "Rank (Obsolete)": "267441",
  "Enable Automatic Patch Review": "false",
  "Work Ratio": 0,
  "Σ Remaining Estimate": 86400,
  "Issue & fix info": null,
  "Affects Version/s": [],
  "Due Date": null,
  "Fix Version/s": []
 },
 "renderedFields": null,
 "changelog": {
  "startAt": 0,
  "maxResults": 1,
  "total": 1,
  "histories": [
   {
    "id": "12791647",
    "author": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=btsulliv",
     "name": "btsulliv",
     "key": "btsulliv",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Blake Sullivan",
     "active": true,
     "timeZone": "America/Los_Angeles"
    },
    "created": "2009-11-30T18:03:01.771+0000",
    "items": [
     {
      "field": "resolution",
      "fieldtype": "jira",
      "from": null,
      "fromString": null,
      "to": "6",
      "toString": "Invalid"
     },
     {
      "field": "status",
      "fieldtype": "jira",
      "from": "1",
      "fromString": "Open",
      "to": "6",
      "toString": "Closed"
     }
    ]
   }
  ]
 },
 "polar_updated": "2009-11-30T18:03:01.000+0000",
 "polar_system": {
  "assignee": null,
  "reporter": {
   "self": "https://issues.apache.org/jira/rest/api/2/user?username=btsulliv",
   "name": "btsulliv",
   "key": "btsulliv",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
    "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
    "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
    "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
   },
   "displayName": "Blake Sullivan",
   "active": true,
   "timeZone": "America/Los_Angeles"
  },
  "creator": {
   "self": "https://issues.apache.org/jira/rest/api/2/user?username=btsulliv",
   "name": "btsulliv",
   "key": "btsulliv",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
    "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
    "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
    "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
   },
   "displayName": "Blake Sullivan",
   "active": true,
   "timeZone": "America/Los_Angeles"
  },
  "issuetype": {
   "self": "https://issues.apache.org/jira/rest/api/2/issuetype/1",
   "id": "1",
   "description": "A problem which impairs or prevents the functions of the product.",
   "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21133&avatarType=issuetype",
   "name": "Bug",
   "subtask": false,
   "avatarId": 21133
  },
  "project": {
   "self": "https://issues.apache.org/jira/rest/api/2/project/12311010",
   "id": "12311010",
   "key": "COMDEV",
   "name": "Community Development",
   "projectTypeKey": "software",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/projectavatar?pid=12311010&avatarId=10011",
    "24x24": "https://issues.apache.org/jira/secure/projectavatar?size=small&pid=12311010&avatarId=10011",
    "16x16": "https://issues.apache.org/jira/secure/projectavatar?size=xsmall&pid=12311010&avatarId=10011",
    "32x32": "https://issues.apache.org/jira/secure/projectavatar?size=medium&pid=12311010&avatarId=10011"
   },
   "projectCategory": {
    "self": "https://issues.apache.org/jira/rest/api/2/projectCategory/10130",
    "id": "10130",
    "description": "General ASF-wide projects.",
    "name": "Apache Software Foundation"
   }
  }
 }
}"##,
    r##"{
 "expand": "operations,versionedRepresentations,editmeta,changelog,renderedFields",
 "id": "12442326",
 "self": "https://issues.apache.org/jira/rest/api/2/issue/12442326",
 "key": "COMDEV-4",
 "fields": {
  "Attachment count": "0.0",
  "Created": "2009-12-03T09:59:30.000+0000",
  "Σ Time Spent": null,
  "Description": "I was trying to parse a XML Document using DOM Parser.\n\nI got null pointer exception while executing doc = builder.parse(xmlDataFile);\n\nThere were few entities in the XML data file. On removing a particular entity, i was able to parse the file successfully\n\nThe entity was some thing like this\n\n<!ENTITY SAMPLE.TIF SYSTEM \"SAMPLE.TIF\" NDATA TIF>\n\nWhat could be the reason?\n\nEDIT:\n\nHere is the code:\n\n        DocumentBuilderFactory aFactory = DocumentBuilderFactory.newInstance();\n        aFactory.setValidating(false);\n        aFactory.setFeature(\"http://xml.org/sax/features/namespaces\", false);\n        aFactory.setFeature(\"http://apache.org/xml/features/validation/schema\", false);\n        aFactory.setIgnoringComments(true);\n        builder = aFactory.newDocumentBuilder();\n        doc = builder.parse(xmlDataFile);\nThis is the trace:\n\n at com.sun.org.apache.xerces.internal.dom.DeferredDocumentImpl.setChunkIndex(DeferredDocumentImpl.java:1944)\n    at com.sun.org.apache.xerces.internal.dom.DeferredDocumentImpl.appendChild(DeferredDocumentImpl.java:644)\n    at com.sun.org.apache.xerces.internal.parsers.AbstractDOMParser.characters(AbstractDOMParser.java:1191)\n    at com.sun.org.apache.xerces.internal.impl.dtd.XMLDTDValidator.characters(XMLDTDValidator.java:862)\n    at com.sun.org.apache.xerces.internal.impl.XMLDocumentFragmentScannerImpl.scanDocument(XMLDocumentFragmentScannerImpl.java:463)\n    at com.sun.org.apache.xerces.internal.parsers.XML11Configuration.parse(XML11Configuration.java:807)\n    at com.sun.org.apache.xerces.internal.parsers.XML11Configuration.parse(XML11Configuration.java:737)\n    at com.sun.org.apache.xerces.internal.parsers.XMLParser.parse(XMLParser.java:107)\n    at com.sun.org.apache.xerces.internal.parsers.DOMParser.parse(DOMParser.java:225)\n    at com.sun.org.apache.xerces.internal.jaxp.DocumentBuilderImpl.parse(DocumentBuilderImpl.java:283)\n    at javax.xml.parsers.DocumentBuilder.parse(DocumentBuilder.java:180)\n    at XMLParser.Parse(XMLParser.java:89)\n    at Main.main(Main.java:116)\n\n\n89 line number points to -> doc = builder.parse(xmlDataFile);",
  "Development": "{summaryBean=com.atlassian.jira.plugin.devstatus.rest.SummaryBean@41b56f95[summary={pullrequest=com.atlassian.jira.plugi...",
  "Original Estimate": null,
  "Rank": "0|i1a60f:",
  "Remaining Estimate": null,
  "Votes": {
   "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-4/votes",
   "votes": 0,
   "hasVoted": false
  },
  "Watchers": {
   "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-4/watchers",
   "watchCount": 0,
   "isWatching": false
  },
  "Component/s": [],
  "Resolution": {
   "self": "https://issues.apache.org/jira/rest/api/2/resolution/1",
   "id": "1",
   "description": "A fix for this issue is checked into the tree and tested.",
   "name": "Fixed"
  },
  "Summary": "External entity in XML causing null pointer exception during DocumentBuilder.parse",
  "Resolved": "2009-12-04T12:22:31.000+0000",
  "Priority": {
   "self": "https://issues.apache.org/jira/rest/api/2/priority/3",
   "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/major.svg",
   "name": "Major",
   "id": "3"
  },
  "Global Rank": "110424",
  "Linked Issues": [
   {
    "id": "12328123",
    "self": "https://issues.apache.org/jira/rest/api/2/issueLink/12328123",
    "outwardIssue": {
     "id": "21720",
     "key": "XERCESJ-977",
     "self": "https://issues.apache.org/jira/rest/api/2/issue/21720",
     "fields": {
      "summary": "Null pointer exception during DOM parsing",
      "status": {
       "self": "https://issues.apache.org/jira/rest/api/2/status/5",
       "description": "A resolution has been taken, and it is awaiting verification by reporter. From here issues are either reopened, or are closed.",
       "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/resolved.png",
       "name": "Resolved",
       "id": "5",
       "statusCategory": {
        "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
        "id": 3,
        "key": "done",
        "colorName": "success",
        "name": "Done"
       }
      },
      "priority": {
       "self": "https://issues.apache.org/jira/rest/api/2/priority/3",
       "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/major.svg",
       "name": "Major",
       "id": "3"
      },
      "issuetype": {
       "self": "https://issues.apache.org/jira/rest/api/2/issuetype/1",
       "id": "1",
       "description": "A problem which impairs or prevents the functions of the product.",
       "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21133&avatarType=issuetype",
       "name": "Bug",
       "subtask": false,
       "avatarId": 21133
      }
     }
    },
    "type": {
     "id": "10030",
     "name": "Reference",
     "inward": "is related to",
     "outward": "relates to",
     "self": "https://issues.apache.org/jira/rest/api/2/issueLinkType/10030"
    }
   }
  ],
  "Time Spent": null,
  "Updated": "2009-12-04T12:22:31.000+0000",
  "Σ Original Estimate": null,
  "Status": {
   "self": "https://issues.apache.org/jira/rest/api/2/status/6",
   "description": "The issue is considered finished, the resolution is correct. Issues which are not closed can be reopened.",
   "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/closed.png",
   "name": "Closed",
   "id": "6",
   "statusCategory": {
    "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
    "id": 3,
    "key": "done",
    "colorName": "success",
    "name": "Done"
   }
  },
  "Sub-Tasks": [],
  "Σ Progress": {
   "progress": 0,
   "total": 0
  },
  "Last Viewed": null,
  "Project": null,
  "Progress": {
   "progress": 0,
   "total": 0
  },
  "Environment": null,
  "Labels": [],
  "Last public comment date": "Fri Dec 04 12:22:31 UTC 2009",
  "Rank (Obsolete)": "267440",
  "Enable Automatic Patch Review": "false",
  "Work Ratio": -1,
  "Σ Remaining Estimate": null,
  "Issue & fix info": null,
  "Affects Version/s": [],
  "Due Date": null,
  "Fix Version/s": []
 },
 "renderedFields": null,
 "changelog": {
  "startAt": 0,
  "maxResults": 2,
  "total": 2,
  "histories": [
   {
    "id": "12793179",
    "author": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=sri_kumar_4u",
     "name": "sri_kumar_4u",
     "key": "sri_kumar_4u",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "sri kumar ",
     "active": false,
     "timeZone": "Etc/UTC"
    },
    "created": "2009-12-04T11:38:05.962+0000",
    "items": [
     {
      "field": "Link",
      "fieldtype": "jira",
      "from": null,
      "fromString": null,
      "to": "XERCESJ-977",
      "toString": "This issue relates to XERCESJ-977"
     }
    ]
   },
   {
    "id": "12793186",
    "author": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=rgardler",
     "name": "rgardler",
     "key": "rgardler",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Ross Gardler",
     "active": true,
     "timeZone": "America/Los_Angeles"
    },
    "created": "2009-12-04T12:22:31.213+0000",
    "items": [
     {
      "field": "assignee",
      "fieldtype": "jira",
      "from": null,
      "fromString": null,
      "to": "rgardler",
      "toString": "Ross Gardler"
     },
     {
      "field": "resolution",
      "fieldtype": "jira",
      "from": null,
      "fromString": null,
      "to": "1",
      "toString": "Fixed"
     },
     {
      "field": "status",
      "fieldtype": "jira",
      "from": "1",
      "fromString": "Open",
      "to": "6",
      "toString": "Closed"
     }
    ]
   }
  ]
 },
 "polar_updated": "2009-12-04T12:22:31.000+0000",
 "polar_system": {
  "assignee": {
   "self": "https://issues.apache.org/jira/rest/api/2/user?username=rgardler",
   "name": "rgardler",
   "key": "rgardler",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
    "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
    "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
    "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
   },
   "displayName": "Ross Gardler",
   "active": true,
   "timeZone": "America/Los_Angeles"
  },
  "reporter": {
   "self": "https://issues.apache.org/jira/rest/api/2/user?username=sri_kumar_4u",
   "name": "sri_kumar_4u",
   "key": "sri_kumar_4u",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
    "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
    "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
    "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
   },
   "displayName": "sri kumar ",
   "active": false,
   "timeZone": "Etc/UTC"
  },
  "creator": {
   "self": "https://issues.apache.org/jira/rest/api/2/user?username=sri_kumar_4u",
   "name": "sri_kumar_4u",
   "key": "sri_kumar_4u",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
    "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
    "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
    "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
   },
   "displayName": "sri kumar ",
   "active": false,
   "timeZone": "Etc/UTC"
  },
  "issuetype": {
   "self": "https://issues.apache.org/jira/rest/api/2/issuetype/11",
   "id": "11",
   "description": "A formal question. Initially added for the Legal JIRA.",
   "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21130&avatarType=issuetype",
   "name": "Question",
   "subtask": false,
   "avatarId": 21130
  },
  "project": {
   "self": "https://issues.apache.org/jira/rest/api/2/project/12311010",
   "id": "12311010",
   "key": "COMDEV",
   "name": "Community Development",
   "projectTypeKey": "software",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/projectavatar?pid=12311010&avatarId=10011",
    "24x24": "https://issues.apache.org/jira/secure/projectavatar?size=small&pid=12311010&avatarId=10011",
    "16x16": "https://issues.apache.org/jira/secure/projectavatar?size=xsmall&pid=12311010&avatarId=10011",
    "32x32": "https://issues.apache.org/jira/secure/projectavatar?size=medium&pid=12311010&avatarId=10011"
   },
   "projectCategory": {
    "self": "https://issues.apache.org/jira/rest/api/2/projectCategory/10130",
    "id": "10130",
    "description": "General ASF-wide projects.",
    "name": "Apache Software Foundation"
   }
  }
 }
}"##,
    r##"{
 "expand": "operations,versionedRepresentations,editmeta,changelog,renderedFields",
 "id": "12442579",
 "self": "https://issues.apache.org/jira/rest/api/2/issue/12442579",
 "key": "COMDEV-5",
 "fields": {
  "Attachment count": "0.0",
  "Created": "2009-12-06T17:15:27.000+0000",
  "Σ Time Spent": null,
  "Description": "Driver for running FSMBuilder for the job history logs, DataNode ClientTrace messages, and TaskTracker ClientTrace messages. Takes input locations of log files and output location to write to. Calls the FSMBuilder class, which then returns calling conventions if insufficient parameters are specified.",
  "Development": "{summaryBean=com.atlassian.jira.plugin.devstatus.rest.SummaryBean@74e8c92f[summary={pullrequest=com.atlassian.jira.plugi...",
  "Original Estimate": null,
  "Rank": "0|i1a607:",
  "Remaining Estimate": null,
  "Votes": {
   "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-5/votes",
   "votes": 0,
   "hasVoted": false
  },
  "Watchers": {
   "self": "https://issues.apache.org/jira/rest/api/2/issue/COMDEV-5/watchers",
   "watchCount": 0,
   "isWatching": false
  },
  "Component/s": [],
  "Resolution": {
   "self": "https://issues.apache.org/jira/rest/api/2/resolution/6",
   "id": "6",
   "description": "The problem isn't valid and it can't be fixed.",
   "name": "Invalid"
  },
  "Summary": "FSMBuilder driver script",
  "Resolved": "2009-12-06T17:15:56.000+0000",
  "Priority": {
   "self": "https://issues.apache.org/jira/rest/api/2/priority/4",
   "iconUrl": "https://issues.apache.org/jira/images/icons/priorities/minor.svg",
   "name": "Minor",
   "id": "4"
  },
  "Global Rank": "110425",
  "Linked Issues": [],
  "Time Spent": null,
  "Updated": "2009-12-06T17:15:56.000+0000",
  "Σ Original Estimate": null,
  "Status": {
   "self": "https://issues.apache.org/jira/rest/api/2/status/6",
   "description": "The issue is considered finished, the resolution is correct. Issues which are not closed can be reopened.",
   "iconUrl": "https://issues.apache.org/jira/images/icons/statuses/closed.png",
   "name": "Closed",
   "id": "6",
   "statusCategory": {
    "self": "https://issues.apache.org/jira/rest/api/2/statuscategory/3",
    "id": 3,
    "key": "done",
    "colorName": "success",
    "name": "Done"
   }
  },
  "Sub-Tasks": [],
  "Σ Progress": {
   "progress": 0,
   "total": 0
  },
  "Last Viewed": null,
  "Project": null,
  "Progress": {
   "progress": 0,
   "total": 0
  },
  "Environment": null,
  "Labels": [],
  "Last public comment date": "Sun Dec 06 17:15:56 UTC 2009",
  "Rank (Obsolete)": "267439",
  "Enable Automatic Patch Review": "false",
  "Work Ratio": -1,
  "Σ Remaining Estimate": null,
  "Issue & fix info": null,
  "Affects Version/s": [],
  "Due Date": null,
  "Fix Version/s": []
 },
 "renderedFields": null,
 "changelog": {
  "startAt": 0,
  "maxResults": 1,
  "total": 1,
  "histories": [
   {
    "id": "12793875",
    "author": {
     "self": "https://issues.apache.org/jira/rest/api/2/user?username=tanjiaqi",
     "name": "tanjiaqi",
     "key": "tanjiaqi",
     "avatarUrls": {
      "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
      "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
      "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
      "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
     },
     "displayName": "Jiaqi Tan",
     "active": true,
     "timeZone": "Etc/UTC"
    },
    "created": "2009-12-06T17:15:56.934+0000",
    "items": [
     {
      "field": "resolution",
      "fieldtype": "jira",
      "from": null,
      "fromString": null,
      "to": "6",
      "toString": "Invalid"
     },
     {
      "field": "status",
      "fieldtype": "jira",
      "from": "1",
      "fromString": "Open",
      "to": "6",
      "toString": "Closed"
     }
    ]
   }
  ]
 },
 "polar_updated": "2009-12-06T17:15:56.000+0000",
 "polar_system": {
  "assignee": null,
  "reporter": {
   "self": "https://issues.apache.org/jira/rest/api/2/user?username=tanjiaqi",
   "name": "tanjiaqi",
   "key": "tanjiaqi",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
    "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
    "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
    "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
   },
   "displayName": "Jiaqi Tan",
   "active": true,
   "timeZone": "Etc/UTC"
  },
  "creator": {
   "self": "https://issues.apache.org/jira/rest/api/2/user?username=tanjiaqi",
   "name": "tanjiaqi",
   "key": "tanjiaqi",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/useravatar?avatarId=10452",
    "24x24": "https://issues.apache.org/jira/secure/useravatar?size=small&avatarId=10452",
    "16x16": "https://issues.apache.org/jira/secure/useravatar?size=xsmall&avatarId=10452",
    "32x32": "https://issues.apache.org/jira/secure/useravatar?size=medium&avatarId=10452"
   },
   "displayName": "Jiaqi Tan",
   "active": true,
   "timeZone": "Etc/UTC"
  },
  "issuetype": {
   "self": "https://issues.apache.org/jira/rest/api/2/issuetype/4",
   "id": "4",
   "description": "An improvement or enhancement to an existing feature or task.",
   "iconUrl": "https://issues.apache.org/jira/secure/viewavatar?size=xsmall&avatarId=21140&avatarType=issuetype",
   "name": "Improvement",
   "subtask": false,
   "avatarId": 21140
  },
  "project": {
   "self": "https://issues.apache.org/jira/rest/api/2/project/12311010",
   "id": "12311010",
   "key": "COMDEV",
   "name": "Community Development",
   "projectTypeKey": "software",
   "avatarUrls": {
    "48x48": "https://issues.apache.org/jira/secure/projectavatar?pid=12311010&avatarId=10011",
    "24x24": "https://issues.apache.org/jira/secure/projectavatar?size=small&pid=12311010&avatarId=10011",
    "16x16": "https://issues.apache.org/jira/secure/projectavatar?size=xsmall&pid=12311010&avatarId=10011",
    "32x32": "https://issues.apache.org/jira/secure/projectavatar?size=medium&pid=12311010&avatarId=10011"
   },
   "projectCategory": {
    "self": "https://issues.apache.org/jira/rest/api/2/projectCategory/10130",
    "id": "10130",
    "description": "General ASF-wide projects.",
    "name": "Apache Software Foundation"
   }
  }
 }
}"##,
];
