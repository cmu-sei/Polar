# ---------------------------------------------------------------------------
# training_projections.py
#
# Builds per-issue accrual training data from one or more historical PIs,
# computes summary statistics, and supports recency-weighted percentiles so
# that more recent increments contribute more to the PERT velocity model.
#
# Key changes from the previous version
# --------------------------------------
# 1. `increment` is now tracked as a column in every training row so you can
#    see (and filter) which PI each observation came from.
# 2. `_assign_recency_weights()` adds a `recency_weight` column using an
#    exponential decay — most recent PI = 1.0, each prior PI * (1 - decay).
# 3. `get_weighted_statistics()` uses row-replication via Polars `repeat_by`
#    to produce decay-weighted percentiles rather than unweighted ones.
# 4. `return_stats_df_display()` and `return_stats_df()` both accept a
#    `recency_decay` parameter (default 0.0 = equal weighting, backward compat).
# ---------------------------------------------------------------------------

import os
import datetime
from copy import deepcopy
import polars as pl
import polars.selectors as cs
import neo4j
import neo4j.time
from utils.neo4j_client import get_driver
from data_transformation_directory.WorkItem.base_item import BaseItem # type: ignore
from data_transformation_directory.WorkItem.jira_map import IN_PROGRESS_STATE # type: ignore
import time
import inspect
from typing import Any, Tuple


key_pair = {}
MAX_SIZE  = 10_000

STATE_LIST  = ['Planned', 'In Progress', 'Verifying', 'Validating', 'Acceptance', 'Done']
STATE_INDEX = {state: idx for idx, state in enumerate(STATE_LIST)}





# ---------------------------------------------------------------------------
# Data fetching
# ---------------------------------------------------------------------------

def get_training_data(
    selected_team: str,
    selected_pis:  list[str],
    just_stories:  bool,
) -> pl.DataFrame:
    """
    Fetch raw transition data for all issues across every PI in selected_pis.

    The returned DataFrame includes an `increment` column so downstream
    functions can apply per-PI recency weights.
    """
    start_time = time.time()

    if just_stories:
        type_match  = "MATCH (i)-[:HAS_TYPE]->(it:JiraIssueType)"
        type_filter = '\nAND it.name = "Story"'
        type_return = "it.name AS issue_type,"
    else:
        type_match  = "OPTIONAL MATCH (i)-[:HAS_TYPE]->(it:IssueType)"
        type_filter = ""
        type_return = "it.name AS issue_type,"

    cypher_query = f'''
        MATCH (t:JiraIssue_Agile_Team)<-[:HAS_AGILE_TEAM]-(i:JiraIssue)
        MATCH (i)-[:HAS_PROGRAM_INCREMENT]->(pi:JiraIssue_Program_Increment)
        MATCH (i)-[:Transitioned]->(tr:JiraIssueChangeLog)
        {type_match}
        MATCH (i)-[:HAS_STATUS]->(st:JiraIssue_Status)
        WHERE t.value = "Thestrals"
        AND pi.value IN $selected_pis
        AND tr.field = "status"
        {type_filter}

        WITH i, tr, it, pi
        ORDER BY i.key, tr.created

        WITH i, it, pi,
            collect(tr) AS transitions,
            ["In Progress", "Verifying", "Validating", "Acceptance", "Done"] AS keyStates, 
            ["In Progress", "Verifying", "Validating", "Acceptance"] AS activeStates

       UNWIND range(0, size(transitions) - 1) AS idx
        WITH i, it, pi, keyStates, activeStates, transitions, idx,
            transitions[idx] AS tr,
            CASE WHEN idx = size(transitions) - 1
                THEN null
                ELSE transitions[idx + 1] END AS next_tr,
            [x IN range(0, idx)
                WHERE transitions[x].toString IN keyStates
                | transitions[x].toString] AS keyHits

        WITH i, it, pi, activeStates, tr, next_tr, keyHits,
            CASE WHEN next_tr IS NULL THEN null
                ELSE duration.inSeconds(datetime(tr.created), datetime(next_tr.created)).seconds
            END AS DeltaTime

        WITH i, it, pi, activeStates, tr, next_tr, keyHits, DeltaTime,
            CASE WHEN DeltaTime IS NULL THEN null
                ELSE round(DeltaTime / 86400.0, 5)
            END AS DeltaDecDay

        RETURN
            i.key            AS issue_key,
            i.`Story Points` AS story_points,
            pi.value         AS increment,
            it.name          AS issue_type,
            tr.fromString    AS prior_state,
            tr.toString      AS current_state,
            tr.baseId        AS transition_id,
            tr.created       AS transition_time,
            DeltaTime,
            DeltaDecDay,
            CASE WHEN i.`Story Points` IS NULL
                   OR i.`Story Points` = 0
                   OR DeltaDecDay IS NULL
                THEN null
                ELSE round(DeltaDecDay / toFloat(i.`Story Points`), 5)
            END              AS days_per_story_point,
            tr.toString IN activeStates AS active,
            CASE WHEN size(keyHits) = 0 THEN null
                ELSE keyHits[-1]
            END              AS logical_state
        ORDER BY issue_key, transition_time
        '''

    driver = get_driver()
    with driver.session() as session:
        result = session.run(
            cypher_query,
            {"team_name": selected_team, "selected_pis": selected_pis},
        )
        df = pl.DataFrame([r.data() for r in result])

    print(
        f"get_training_data: {len(df)} rows across "
        f"{df['increment'].n_unique() if not df.is_empty() else 0} PIs "
        f"({time.time() - start_time:.2f}s)"
    )
    return df


# ---------------------------------------------------------------------------
# Per-issue accrual builder
# ---------------------------------------------------------------------------

def get_active_dec_days(df: pl.DataFrame, issue_key: str) -> float:
    """
    Total DeltaDecDay across all rows for a given issue where active = true.
    Returns 0.0 if the issue has no active rows.
    """
    total = (
        df
        .filter((pl.col("issue_key") == issue_key) & pl.col("active"))
        .select(pl.col("DeltaDecDay").sum())
        .item()
    )
    return total if total is not None else 0.0

# ---------------------------------------------------------------------------
# Recency weighting
# ---------------------------------------------------------------------------

def _assign_recency_weights(
    df:            pl.DataFrame,
    increment_col: str   = "increment",
    decay:         float = 0.5,
) -> pl.DataFrame:
    """
    Add a `recency_weight` column using exponential decay.

    Increments are sorted alphabetically descending (most recent PI label
    last alphabetically = highest weight).  Adjust if your PI naming
    convention sorts differently — e.g. pass a pre-sorted list explicitly.

    Parameters
    ----------
    decay : float in [0, 1)
        0.0 → all PIs equally weighted (backward compatible).
        0.5 → each prior PI is worth half the next-newer one.
        0.8 → heavy recency bias.

    Returns
    -------
    df with an added `recency_weight` column (Float64, max = 1.0).
    """
    if decay == 0.0:
        return df.with_columns(pl.lit(1.0).alias("recency_weight"))

    # Sort descending so index 0 = most recent
    ordered = (
        df.select(increment_col)
        .unique()
        .sort(increment_col, descending=True)
        [increment_col]
        .to_list()
    )

    weight_map = {pi: (1.0 - decay) ** i for i, pi in enumerate(ordered)}

    return df.with_columns(
        pl.col(increment_col)
          .replace(weight_map)
          .cast(pl.Float64)
          .alias("recency_weight")
    )


# ---------------------------------------------------------------------------
# Statistics
# ---------------------------------------------------------------------------

def get_statistics(df: pl.DataFrame) -> pl.DataFrame:
    """
    Compute unweighted P10/P25/P50/P75/P90 for all numeric accrual columns.
    """
    numeric_df = df.select(cs.numeric())

    percentiles = numeric_df.select(
        [pl.col(c).quantile(0.10).alias(f"{c}_10th") for c in numeric_df.columns] +
        [pl.col(c).quantile(0.25).alias(f"{c}_25th") for c in numeric_df.columns] +
        [pl.col(c).quantile(0.50).alias(f"{c}_50th") for c in numeric_df.columns] +
        [pl.col(c).quantile(0.75).alias(f"{c}_75th") for c in numeric_df.columns] +
        [pl.col(c).quantile(0.90).alias(f"{c}_90th") for c in numeric_df.columns]
    )

    melted = (
        percentiles
        .unpivot(variable_name="Metric", value_name="Value")
        .with_columns([
            pl.col("Metric").str.replace(r"_(10th|25th|50th|75th|90th)$", "").alias("Status"),
            pl.col("Metric").str.extract(r"(10th|25th|50th|75th|90th)$", 1).alias("Percentile"),
        ])
        .drop("Metric")
    )

    stats_df = (
        melted
        .pivot(index="Status", on="Percentile", values="Value")
        .filter(~pl.col("Status").str.contains(r"Planned|Start|End"))
        .select(["Status", "10th", "25th", "50th", "75th", "90th"])
    )

    return stats_df


def get_weighted_statistics(
    df:         pl.DataFrame,
    weight_col: str = "recency_weight",
    scale:      int = 10,
) -> pl.DataFrame:
    if weight_col not in df.columns:
        return get_statistics(df)

    max_w = df[weight_col].max()
    if max_w is None or max_w == 0:
        return get_statistics(df)

    df_rep = df.with_columns(
        ((pl.col(weight_col) / max_w * scale).round(0).cast(pl.Int32).clip(lower_bound=1))
        .alias("_rep")
    )

    df_weighted = (
        df_rep
        .select(pl.exclude([weight_col, "increment", "issue_key", "type"]))
        .select(pl.all().repeat_by("_rep"))
        .explode(pl.all())
        .drop("_rep")               
    )

    return get_statistics(df_weighted)

# ---------------------------------------------------------------------------
# Pipeline builders
# ---------------------------------------------------------------------------

def generate_issues_training_data_list(
    df:   pl.DataFrame,
    team: Any,
) -> pl.DataFrame:
    """
    One row per issue: total active decimal days and total active days
    per story point, keeping increment/type for downstream weighting/stats.
    """
    function_name = inspect.currentframe().f_code.co_name
    start_time    = time.time()

    if df.is_empty():
        return pl.DataFrame()

    out = (
        df
        .group_by(["issue_key", "increment", "issue_type"])
        .agg([
            pl.col("story_points").first(),
            pl.col("DeltaDecDay")
              .filter(pl.col("active"))
              .sum()
              .fill_null(0.0)
              .alias("Active Dec Accrual"),
            pl.col("days_per_story_point")
              .filter(pl.col("active"))
              .sum()
              .alias("days_per_story_point"),
        ])
        # Issues with null/zero SP have null ratios on every row; their sum
        # comes back as 0.0, which would poison the PERT fit's optimistic
        # bound — null them out so _fit_pert_params drops them instead.
        .with_columns(
            pl.when(
                pl.col("story_points").is_null()
                | (pl.col("story_points") <= 0)
            )
            .then(None)
            .otherwise(pl.col("days_per_story_point"))
            .alias("days_per_story_point")
        )
        .rename({"issue_type": "type"})
        .sort("issue_key")
    )

    n_null = out["days_per_story_point"].null_count()
    print(
        f"{function_name}: {len(out)} issues "
        f"({n_null} excluded from days-per-SP) "
        f"in {time.time() - start_time:.2f}s"
    )
    return out

# ---------------------------------------------------------------------------
# Public entry points
# ---------------------------------------------------------------------------

def return_stats_df_display(
    team:          str,
    increments:    list[str],
    just_stories:  bool  = False,
    recency_decay: float = 0.0,
) -> tuple[pl.DataFrame, pl.DataFrame]:
    """
    Fetch training data for all increments, build accrual rows, apply optional
    recency weighting, and return (stats_df, all_issues_df).

    Parameters
    ----------
    recency_decay : float in [0, 1)
        0.0  → equal weighting across all PIs  (default, backward compatible)
        0.5  → each prior PI contributes half as much as the next-newer one
        0.8  → strong recency bias toward the most recent PI
    """
    function_name = inspect.currentframe().f_code.co_name
    start_time    = time.time()

    df             = get_training_data(team, list(set(increments)), just_stories)
    all_issues_df  = generate_issues_training_data_list(df, team)

    if recency_decay > 0.0:
        all_issues_df = _assign_recency_weights(all_issues_df, decay=recency_decay)
        stats_df      = get_weighted_statistics(all_issues_df)
    else:
        stats_df = get_statistics(all_issues_df)

    pi_counts = (
        all_issues_df.group_by("increment").len().sort("increment")
        if "increment" in all_issues_df.columns
        else None
    )
    if pi_counts is not None:
        print(f"[Training] Issues per PI:\n{pi_counts}")

    print(f"{function_name}: {time.time() - start_time:.2f}s")
    return stats_df, all_issues_df


def return_stats_df(
    team:          str,
    increments:    list[str],
    just_stories:  bool  = False,
    recency_decay: float = 0.0,
) -> pl.DataFrame:
    """
    Like return_stats_df_display() but returns only the raw accrual DataFrame
    (with recency_weight column if decay > 0) for use in projection sampling.
    """
    function_name = inspect.currentframe().f_code.co_name
    start_time    = time.time()

    df            = get_training_data(team, list(set(increments)), just_stories)
    all_issues_df = generate_issues_training_data_list(df, team)

    if recency_decay > 0.0:
        all_issues_df = _assign_recency_weights(all_issues_df, decay=recency_decay)

    print(f"{function_name}: {time.time() - start_time:.2f}s")
    return all_issues_df


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main():
    function_name = inspect.currentframe().f_code.co_name
    start_time    = time.time()

    team = "Thestrals"
    current_increment = "PI-32"
    training_increments = ["PI-32", "PI-31", "PI-30"]

    df = get_training_data(team, training_increments, just_stories=True)
    print(df)
    # recency_decay=0.5 → PI-32 weight 1.0, PI-30 weight 0.5, PI-28 weight 0.25
    stats_df, all_issues_df = return_stats_df_display(
        team, training_increments, just_stories=True, recency_decay=0.5
    )

    print("\n── Per-PI issue counts ───────────────────────")
    if "increment" in all_issues_df.columns:
        print(all_issues_df.group_by("increment").len().sort("increment"))

    print("\n── Weighted statistics (Active Dec Accrual) ──")
    print(stats_df.filter(pl.col("Status").str.contains("days_per_story_point")))

    # Return the raw df for use in build_projections() training_df argument
    training_df = return_stats_df(
        team, training_increments, just_stories=True, recency_decay=0.5
    )

    print(f"\n{function_name}: {time.time() - start_time:.2f}s total")




if __name__ == "__main__":
    main()
