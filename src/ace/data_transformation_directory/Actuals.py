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
# actuals.py
#
# Tracks actual completion and in-progress status from Jira transition data.
# Two concerns live here because they share the same event-aggregation pattern:
#
#   build_pi_actuals()     — daily cumulative SP series for stories
#   build_feature_actuals()— one-row-per-feature start/done summary
# ---------------------------------------------------------------------------

import datetime
from datetime import timedelta

import polars as pl

from data_transformation_directory.constants import DONE_STATES
from data_transformation_directory.Db import _normalize_to_naive


# ---------------------------------------------------------------------------
# Transition date extractors
# ---------------------------------------------------------------------------

def _get_done_date(transitions_df: pl.DataFrame) -> datetime.datetime | None:
    """
    Return the FIRST timestamp an issue entered a DONE_STATES state.
    Returns None if the issue has not yet reached Done.
    """
    if transitions_df is None or transitions_df.is_empty():
        return None
    done_rows = (
        transitions_df
        .filter(pl.col("jira_state").is_in(list(DONE_STATES)))
        .sort("start")
    )
    return _normalize_to_naive(done_rows["start"][-1]) if not done_rows.is_empty() else None


def _get_start_date(transitions_df: pl.DataFrame) -> datetime.datetime | None:
    """
    Return the FIRST timestamp an issue entered In Progress.
    Returns None if the issue has not yet started.
    """
    if transitions_df is None or transitions_df.is_empty():
        return None
    start_rows = (
        transitions_df
        .filter(pl.col("jira_state") == "In Progress")
        .sort("start")
    )
    return _normalize_to_naive(start_rows["start"][0]) if not start_rows.is_empty() else None

def _collapse_consecutive_runs(dates: list[datetime.datetime]) -> list[datetime.datetime]:
    """
    Given a sorted list of dates, collapse consecutive-day runs down to
    just the first date of each run.

    e.g. [5/5, 5/6, 5/20, 5/21, 5/25, 6/1, 6/2, 6/3, 6/4]
      -> [5/5, 5/20, 5/25, 6/1]
    """
    if not dates:
        return []

    dates = sorted(dates)
    collapsed = [dates[0]]

    for prev, curr in zip(dates, dates[1:]):
        if (curr - prev).days > 1:
            collapsed.append(curr)

    return collapsed


# ---------------------------------------------------------------------------
# Story actuals
# ---------------------------------------------------------------------------

def build_pi_actuals(all_issues: list) -> pl.DataFrame:
    """
    Build a daily cumulative-completion and in-progress series.

    Both series are monotonically increasing cumulative sums:
        completed_sp   — SP that have hit a Done state by end of day
        in_progress_sp — SP that have ever entered In Progress by end of day

    Returns
    -------
    pl.DataFrame — columns:
        date, completed_sp, completed_pct, issues_completed,
        in_progress_sp, in_progress_pct, issues_in_progress
    """
    total_sp = sum(item.weight for item in all_issues if item.weight)

    if total_sp == 0:
        raise ValueError(
            "build_pi_actuals: total story points is 0. "
            "Ensure issues have story_points populated in Neo4j."
        )

    _EMPTY_SCHEMA = {
        "date":               pl.Datetime("us"),
        "completed_sp":       pl.Float64,
        "completed_pct":      pl.Float64,
        "issues_completed":   pl.Int32,
        "in_progress_sp":     pl.Float64,
        "in_progress_pct":    pl.Float64,
        "issues_in_progress": pl.Int32,
    }

    # ── Single pass: collect done and in-progress events ──────────────────────
    completion_events: list[dict] = []
    start_events:      list[dict] = []

    for item in all_issues:
        sp       = float(item.weight) if item.weight else 0.0
        done_dt  = _get_done_date(item.transitions)
        start_dt = _get_start_date(item.transitions)

        if done_dt is not None:
            completion_events.append({
                "issue_key":    item.key,
                "done_day":     done_dt.replace(hour=0, minute=0, second=0, microsecond=0),
                "story_points": sp,
            })
        if start_dt is not None:
            start_events.append({
                "issue_key":    item.key,
                "start_day":    start_dt.replace(hour=0, minute=0, second=0, microsecond=0),
                "story_points": sp,
            })

    if not completion_events and not start_events:
        msg = "[Actuals] No issues have started or completed yet."
        print(msg)
        return pl.DataFrame(schema=_EMPTY_SCHEMA), msg

    # ── Aggregate per-day totals ───────────────────────────────────────────────
    def _agg(events: list[dict], day_col: str) -> pl.DataFrame:
        if not events:
            return pl.DataFrame(schema={
                day_col: pl.Datetime("us"), "sp_on_day": pl.Float64, "issues_on_day": pl.Int32,
            })
        return (
            pl.DataFrame(events)
            .with_columns(pl.col(day_col).cast(pl.Datetime("us")))
            .group_by(day_col)
            .agg([
                pl.col("story_points").sum().alias("sp_on_day"),
                pl.len().cast(pl.Int32).alias("issues_on_day"),
            ])
            .sort(day_col)
        )

    done_agg  = _agg(completion_events, "done_day")
    start_agg = _agg(start_events,      "start_day")

    # ── Build continuous daily spine ──────────────────────────────────────────
    all_dates = [e["done_day"] for e in completion_events] + [e["start_day"] for e in start_events]
    first_day = min(all_dates)
    today     = datetime.datetime.now().replace(hour=0, minute=0, second=0, microsecond=0)
    last_day  = max(max(all_dates), today)

    spine_df = pl.DataFrame(
        [{"date": first_day + timedelta(days=i)} for i in range((last_day - first_day).days + 1)]
    ).with_columns(pl.col("date").cast(pl.Datetime("us")))

    # ── Join, fill nulls, accumulate ──────────────────────────────────────────
    actuals_df = (
        spine_df
        .join(done_agg.rename({
            "done_day": "date", "sp_on_day": "sp_completed_on_day",
            "issues_on_day": "issues_completed_on_day",
        }), on="date", how="left")
        .join(start_agg.rename({
            "start_day": "date", "sp_on_day": "sp_started_on_day",
            "issues_on_day": "issues_started_on_day",
        }), on="date", how="left")
        .with_columns([
            pl.col("sp_completed_on_day").fill_null(0.0),
            pl.col("issues_completed_on_day").fill_null(0).cast(pl.Int32),
            pl.col("sp_started_on_day").fill_null(0.0),
            pl.col("issues_started_on_day").fill_null(0).cast(pl.Int32),
        ])
        .with_columns([
            pl.col("sp_completed_on_day").cum_sum().alias("completed_sp"),
            pl.col("issues_completed_on_day").cum_sum().alias("issues_completed"),
            pl.col("sp_started_on_day").cum_sum().alias("in_progress_sp"),
            pl.col("issues_started_on_day").cum_sum().alias("issues_in_progress"),
        ])
        .with_columns([
            (pl.col("completed_sp")   / total_sp * 100).round(2).alias("completed_pct"),
            (pl.col("in_progress_sp") / total_sp * 100).round(2).alias("in_progress_pct"),
        ])
        .select([
            "date",
            "completed_sp",   "completed_pct",   "issues_completed",
            "in_progress_sp", "in_progress_pct", "issues_in_progress",
        ])
    )

    WIP_LIMIT = 20.0
    wip_dates = actuals_df.filter(
        (pl.col('in_progress_pct') - pl.col('completed_pct')) > WIP_LIMIT
    )['date'].cast(pl.Datetime("us")).to_list()

    wip_dates = _collapse_consecutive_runs(wip_dates)

    print("Dates where WIP gap exceeds limit:")
    print(wip_dates)


    return_string = (
        f"{len(completion_events)}/{len(all_issues)} issues complete "
        f"| {actuals_df['completed_sp'][-1]:.1f} / {total_sp} SP "
        f"({actuals_df['completed_pct'][-1]:.1f}%) "
        f"| {len(start_events)} issues ever started"
    )
    return actuals_df, return_string, wip_dates
