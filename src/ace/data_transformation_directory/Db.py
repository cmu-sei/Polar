# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# ---------------------------------------------------------------------------
# db.py
#
# Neo4j connection setup, datetime normalisation helpers, and all raw data
# fetching functions.  Nothing in here knows about BaseItem or PI logic --
# it only speaks Neo4j -> Polars DataFrame.
# ---------------------------------------------------------------------------

import os
import datetime
from datetime import timezone

import neo4j.time
import polars as pl

from utils.neo4j_client import get_driver


# ---------------------------------------------------------------------------
# Graph schema configuration
#
# Node labels, relationship types, and property names below are read from
# the environment so this module can point at a differently-shaped Neo4j
# graph without code changes -- see schema_config.env for the reference
# list of variables and their current (default) values.
# ---------------------------------------------------------------------------

TEAM_LABEL           = os.getenv("SCHEMA_TEAM_LABEL", "JiraIssue_Agile_Team")
ISSUE_LABEL          = os.getenv("SCHEMA_ISSUE_LABEL", "JiraIssue")
PI_LABEL             = os.getenv("SCHEMA_PI_LABEL", "JiraIssue_Program_Increment")
CHANGELOG_LABEL      = os.getenv("SCHEMA_CHANGELOG_LABEL", "JiraIssueChangeLog")
ISSUE_TYPE_LABEL     = os.getenv("SCHEMA_ISSUE_TYPE_LABEL", "JiraIssueType")
STATUS_LABEL         = os.getenv("SCHEMA_STATUS_LABEL", "JiraIssue_Status")

HAS_AGILE_TEAM_REL        = os.getenv("SCHEMA_HAS_AGILE_TEAM_REL", "HAS_AGILE_TEAM")
HAS_PROGRAM_INCREMENT_REL = os.getenv("SCHEMA_HAS_PROGRAM_INCREMENT_REL", "HAS_PROGRAM_INCREMENT")
TRANSITIONED_REL          = os.getenv("SCHEMA_TRANSITIONED_REL", "Transitioned")
HAS_TYPE_REL              = os.getenv("SCHEMA_HAS_TYPE_REL", "HAS_TYPE")
HAS_STATUS_REL            = os.getenv("SCHEMA_HAS_STATUS_REL", "HAS_STATUS")
HAS_SPRINT_REL            = os.getenv("SCHEMA_HAS_SPRINT_REL", "HAS_SPRINT")

TEAM_VALUE_PROP      = os.getenv("SCHEMA_TEAM_VALUE_PROP", "value")
PI_VALUE_PROP        = os.getenv("SCHEMA_PI_VALUE_PROP", "value")
ISSUE_KEY_PROP       = os.getenv("SCHEMA_ISSUE_KEY_PROP", "key")
STORY_POINTS_PROP    = os.getenv("SCHEMA_STORY_POINTS_PROP", "Story Points")
ISSUE_TYPE_NAME_PROP = os.getenv("SCHEMA_ISSUE_TYPE_NAME_PROP", "name")
STORY_TYPE_NAME      = os.getenv("SCHEMA_STORY_TYPE_NAME", "Story")


# ---------------------------------------------------------------------------
# Datetime normalisation helpers
# ---------------------------------------------------------------------------

def _to_naive_datetime_expr(col_name: str) -> pl.Expr:
    """Convert an Object/String date-like column to a naive Polars Datetime."""
    return pl.col(col_name).map_elements(
        lambda v: (
            v.to_native().replace(tzinfo=None)
            if isinstance(v, neo4j.time.DateTime)
            else v.replace(tzinfo=None)
            if isinstance(v, datetime.datetime) and v.tzinfo is not None
            else v
            if isinstance(v, datetime.datetime)
            else datetime.datetime.fromisoformat(v.replace("Z", "+00:00")).replace(tzinfo=None)
            if isinstance(v, str)
            else None
        ),
        return_dtype=pl.Datetime,
    )


def _neo_to_py_dt(v) -> datetime.datetime:
    """Normalize a Neo4j or Python datetime into a tz-aware Python datetime."""
    if isinstance(v, neo4j.time.DateTime):
        return v.to_native().replace(tzinfo=timezone.utc)
    if isinstance(v, datetime.datetime):
        return v if v.tzinfo else v.replace(tzinfo=timezone.utc)
    return v


def _normalize_neo_datetime_column(df: pl.DataFrame, colname: str) -> pl.DataFrame:
    """Convert an Object-typed datetime column into a proper Polars Datetime(UTC)."""
    if colname not in df.columns or df[colname].dtype != pl.Object:
        return df
    values = [_neo_to_py_dt(v) for v in df[colname].to_list()]
    return df.with_columns(
        pl.Series(name=colname, values=values, dtype=pl.Datetime(time_zone="UTC"))
    )


def _normalize_to_naive(dt) -> datetime.datetime | None:
    """Coerce any datetime-like value to a naive (tz-stripped) Python datetime."""
    if dt is None:
        return None
    if isinstance(dt, neo4j.time.DateTime):
        dt = dt.to_native()
    if isinstance(dt, datetime.datetime) and dt.tzinfo is not None:
        return dt.replace(tzinfo=None)
    return dt


# ---------------------------------------------------------------------------
# Raw data fetching
# ---------------------------------------------------------------------------

def get_column_python_types(df: pl.DataFrame) -> dict:
    """Return a dict mapping each column name to a count of Python value types found in it."""
    type_summary = {}
    for col in df.columns:
        type_counts: dict = {}
        for v in df[col].drop_nulls().to_list():
            t = type(v)
            type_counts[t] = type_counts.get(t, 0) + 1
        type_summary[col] = type_counts
    return type_summary


def get_data(selected_team: str, selected_pi: str, just_stories: bool, todays_date: str) -> pl.DataFrame:
    """
    Query Neo4j for all issues (or only Stories) for the given team and PI.

    Returns a raw Polars DataFrame -- one row per (issue, transition, sprint)
    combination before any deduplication or object construction.
    """

    date_cutoff = f"{todays_date}T00:00:00.000+0000"

    story_filter = f'\nAND it.{ISSUE_TYPE_NAME_PROP} = "{STORY_TYPE_NAME}"' if just_stories else ""

    cypher_query = f'''
        MATCH (t:{TEAM_LABEL})<-[:{HAS_AGILE_TEAM_REL}]-(i:{ISSUE_LABEL})
        MATCH (i)-[:{HAS_PROGRAM_INCREMENT_REL}]->(pi:{PI_LABEL})
        MATCH (i)-[:{TRANSITIONED_REL}]->(tr:{CHANGELOG_LABEL})
        MATCH (i)-[:{HAS_TYPE_REL}]->(it:{ISSUE_TYPE_LABEL})
        MATCH (i)-[:{HAS_STATUS_REL}]->(st:{STATUS_LABEL})
        WHERE t.{TEAM_VALUE_PROP} = $team_name
        AND tr.created < $date_cutoff
        AND pi.{PI_VALUE_PROP} = $pi_choice
        AND tr.field = "status"
        {story_filter}

        WITH i, tr, it, st, pi, tr.baseId AS transition_id
        ORDER BY i.{ISSUE_KEY_PROP}, tr.created

        RETURN
            i.{ISSUE_KEY_PROP}      AS issue_key,
            i.`{STORY_POINTS_PROP}` AS story_points,
            pi.{PI_VALUE_PROP}      AS increment,
            it.{ISSUE_TYPE_NAME_PROP} AS issue_type,
            tr.fromString    AS prior_state,
            tr.toString      AS state,
            tr.created       AS transition_time,
            transition_id
    '''

    driver = get_driver()
    with driver.session() as session:
        result = session.run(
            cypher_query,
            {"team_name": selected_team, "pi_choice": selected_pi, "date_cutoff": date_cutoff},
        )
        df = pl.DataFrame([r.data() for r in result])
    return df

def get_data_baseline(selected_team: str, selected_pi: str, just_stories: bool) -> pl.DataFrame:
    """
    Query Neo4j for all issues (or only Stories) for the given team and PI.

    Returns a raw Polars DataFrame -- one row per (issue, transition, sprint)
    combination before any deduplication or object construction.
    """
    story_filter = f'\nAND it.{ISSUE_TYPE_NAME_PROP} = "{STORY_TYPE_NAME}"' if just_stories else ""

    cypher_query = f'''
        MATCH (t:{TEAM_LABEL})<-[:{HAS_AGILE_TEAM_REL}]-(i:{ISSUE_LABEL})
        MATCH (i)-[:{HAS_PROGRAM_INCREMENT_REL}]->(pi:{PI_LABEL})
        MATCH (i)-[:{HAS_SPRINT_REL}]->(s:Sprint)
        MATCH (i)-[:{HAS_TYPE_REL}]->(it:{ISSUE_TYPE_LABEL})
        MATCH (i)-[:{HAS_STATUS_REL}]->(st:{STATUS_LABEL})

        WHERE t.{TEAM_VALUE_PROP} = $team_name
        AND pi.{PI_VALUE_PROP} = $pi_choice
        {story_filter}

        WITH i, it, s, st, pi

        RETURN
            i.{ISSUE_KEY_PROP}      AS issue_key,
            i.`{STORY_POINTS_PROP}` AS story_points,
            pi.{PI_VALUE_PROP}      AS increment,
            it.{ISSUE_TYPE_NAME_PROP} AS issue_type,
            s.name           AS name,
            s.startDate      AS startDate,
            s.endDate        AS endDate,
            st.name          AS status_name

        ORDER BY issue_key, name
    '''

    driver = get_driver()
    with driver.session() as session:
        result = session.run(
            cypher_query,
            {"team_name": selected_team, "pi_choice": selected_pi},
        )
        df = pl.DataFrame([r.data() for r in result])
    return df

def get_data_baseline_issues_backlog(selected_team: str, selected_pi: str, current_date: str, just_stories: bool) -> pl.DataFrame:
    """
    Query Neo4j for issues in the baseline backlog "To Do" Status.
    """
    story_filter = f'\nAND it.{ISSUE_TYPE_NAME_PROP} = "{STORY_TYPE_NAME}"' if just_stories else ""

    cypher_query = f'''
        MATCH (t:{TEAM_LABEL})<-[:{HAS_AGILE_TEAM_REL}]-(i:{ISSUE_LABEL})
        MATCH (i)-[:{HAS_PROGRAM_INCREMENT_REL}]->(pi:{PI_LABEL})
        MATCH (i)-[:{HAS_SPRINT_REL}]->(s:Sprint)
        MATCH (i)-[:{HAS_TYPE_REL}]->(it:{ISSUE_TYPE_LABEL})
        MATCH (i)-[:{HAS_STATUS_REL}]->(st:{STATUS_LABEL})
        WHERE t.{TEAM_VALUE_PROP} = $team_name
        AND pi.{PI_VALUE_PROP} = $pi_choice
        {story_filter}
        AND st.name = "To Do"

        WITH i, it, st, pi, s
        ORDER BY s.endDate DESC

        WITH i, it, st, pi, collect(s)[0] AS s

        RETURN
            i.{ISSUE_KEY_PROP}      AS issue_key,
            i.`{STORY_POINTS_PROP}` AS story_points,
            pi.{PI_VALUE_PROP}      AS increment,
            it.{ISSUE_TYPE_NAME_PROP} AS issue_type,
            s.name           AS name,
            s.startDate      AS startDate,
            s.endDate        AS endDate,
            st.name          AS status_name

        ORDER BY startDate, issue_key
    '''

    driver = get_driver()
    with driver.session() as session:
        result = session.run(
            cypher_query,
            {"team_name": selected_team, "pi_choice": selected_pi},
        )
        df = pl.DataFrame([r.data() for r in result])
    return df


def get_issues_list(df: pl.DataFrame) -> list[str]:
    """Return a sorted list of unique issue keys from the raw DataFrame."""
    return df["issue_key"].unique().sort().to_list()
