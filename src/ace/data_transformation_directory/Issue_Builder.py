# ---------------------------------------------------------------------------
# issue_builder.py
#
# Constructs BaseItem objects from the raw Neo4j DataFrame returned by db.py.
# One BaseItem per issue key, holding its sprint list and transition timeline.
# ---------------------------------------------------------------------------

import polars as pl

from data_transformation_directory.WorkItem.base_item import BaseItem
from data_transformation_directory.WorkItem.helper import is_late_story
from data_transformation_directory.Db import (
    get_data,
    get_data_baseline, 
    get_issues_list,
    _to_naive_datetime_expr,
    _normalize_neo_datetime_column,
)


def generate_issue_object_data(
    df: pl.DataFrame,
    issue: str,
    increment_value: str,
    team: str,
    actual: bool, 
) -> BaseItem:
    """
    Build and return a single BaseItem from the rows belonging to one issue key.

    Handles datetime normalisation, Fibonacci story-point rounding, sprint
    deduplication, and transition timeline construction.

    Safe for both full (transitions present) and baseline (no transitions) DataFrames.
    """
    new_df = df.filter(pl.col("issue_key") == issue)

    if actual == True: 
        new_df = new_df.with_columns(
            pl.col("state").last().alias("status_name")
)
    # ── Datetime normalisation ────────────────────────────────────────────────
    existing_date_cols = [
        c for c in ["startDate", "endDate", "transition_time"] if c in new_df.columns
    ]
    if existing_date_cols:
        new_df = new_df.with_columns(
            [_to_naive_datetime_expr(c).alias(c) for c in existing_date_cols]
        )

    for col in ["startDate", "endDate"]:
        if col in new_df.columns and new_df[col].dtype == pl.Datetime:
            new_df = new_df.with_columns(pl.col(col).dt.replace_time_zone("UTC"))

    if "transition_time" in new_df.columns:
        new_df = _normalize_neo_datetime_column(new_df, "transition_time")

    # ── Scalar fields ─────────────────────────────────────────────────────────
    new_item                  = BaseItem()
    new_item.team             = team
    new_item.key              = issue
    new_item.raw_data         = new_df
    new_item.increment        = increment_value 
    # current_state — only meaningful when transitions exist
    has_transitions = (
        "transition_time" in new_df.columns
        and "state" in new_df.columns
        and not new_df["transition_time"].is_null().all()
    )

    if has_transitions:
        latest_row = new_df.sort("transition_time").tail(1)
        new_item.current_state = latest_row["state"][0]
        new_item.last_transition = latest_row["state"][0]

    story_points_vals = new_df["story_points"].drop_nulls().to_list() if "story_points" in new_df.columns else []
    story_points      = story_points_vals[0] if story_points_vals else 0.0
    new_item.weight   = story_points

    new_item.type = (
        new_df["issue_type"].drop_nulls().to_list()[0]
        if "issue_type" in new_df.columns and new_df["issue_type"].drop_nulls().len() > 0
        else ""
    )

    # ── Fibonacci rounding ────────────────────────────────────────────────────
    fib_nums = [float(n) for n in [1, 2, 3, 5, 8, 13, 21, 34, 55, 89]]
    set_max  = float(story_points)
    if story_points not in fib_nums and story_points != 0:
        set_max = 0.0
        for it_count, num in enumerate(fib_nums):
            if it_count + 1 >= len(fib_nums):
                set_max = num
                break
            if story_points < num and it_count != 0:
                set_max = fib_nums[it_count - 1]
                break

    new_item.adjusted_weight = float(set_max)
    new_item.orig_adj_weight = float(set_max)

    # ── Sprint list: best-quality row per sprint name ─────────────────────────
    if "name" in new_df.columns:
        date_quality_cols = [c for c in ["startDate", "endDate"] if c in new_df.columns]
        valid_sprints_df = new_df.filter(pl.col("name").is_not_null())

        if valid_sprints_df.is_empty():
            new_item.sprints = []
        else:
            if date_quality_cols:
                valid_sprints_df = valid_sprints_df.with_columns(
                    pl.sum_horizontal(
                        *[pl.col(col).is_not_null().cast(pl.Int64) for col in date_quality_cols]
                    ).alias("date_quality")
                )
                sprint_records = (
                    valid_sprints_df
                    .sort(["name", "date_quality"], descending=[False, True])
                    .unique(subset=["name"], keep="first")
                    .select([c for c in ["startDate", "endDate", "name"] if c in valid_sprints_df.columns])
                )
            else:
                sprint_records = (
                    valid_sprints_df
                    .unique(subset=["name"], keep="first")
                    .select(["name"])
                )
            new_item.sprints = sprint_records.to_dicts()
    else:
        new_item.sprints = []

    new_item.parent_keys = new_df["parent_keys"] if "parent_keys" in new_df.columns else []

    # ── Transition timeline ───────────────────────────────────────────────────
    empty_transitions = pl.DataFrame(schema={
        "jira_state":    pl.Utf8,
        "logical_state": pl.Utf8,
        "start":         pl.Datetime("us"),
        "stop":          pl.Datetime("us"),
        "DeltaTime":     pl.Float64,
        "DeltaDecDay":   pl.Float64,
        "active":        pl.Boolean,
    })

    if has_transitions:
        new_item.transitions = (
            new_df
            .select(["state", "transition_time"])
            .drop_nulls()
            .unique(subset=["state", "transition_time"], keep="first")
            .sort("transition_time", descending=False)
            .with_columns(
                pl.lit(None).cast(pl.Utf8).alias("logical_state"),
                pl.lit(None).cast(pl.Datetime("us")).alias("stop"),
                pl.lit(0.0).alias("DeltaTime"),
                pl.lit(0.0).alias("DeltaDecDay"),
                pl.lit(False).alias("active"),
                pl.col("transition_time").alias("start"),
            )
            .drop("transition_time")
            .rename({"state": "jira_state"})
        )
    else:
        new_item.transitions = empty_transitions

    return new_item


def generate_issues_list(
    team: str,
    increment_value: str,
    just_stories: bool,
    todays_date: str, 
    baseline:     bool = False, 
) -> list[BaseItem]:
    """
    Fetch raw Neo4j data for the given team/PI and build a list of BaseItem objects.

    This is the main entry point for the rest of the pipeline — every downstream
    function consumes the list this returns.
    """
    if baseline:
        df = get_data_baseline(team, increment_value, just_stories)
        issues = get_issues_list(df)
    else:
        df = get_data(team, increment_value, just_stories, todays_date)
        issues = get_issues_list(df)


    # Ensure transition_time is a proper Datetime before per-issue processing
    if "transition_time" in df.columns and df["transition_time"].dtype != pl.Datetime:
        df = df.with_columns(
            pl.col("transition_time")
              .map_elements(lambda x: x.to_native() if hasattr(x, "to_native") else x)
              .cast(pl.Datetime)
        )

    all_issues = []
    for issue_key in issues:
        if baseline: 
            issue_obj = generate_issue_object_data(df, issue_key, increment_value, team, False)
        else: 
            issue_obj = generate_issue_object_data(df, issue_key, increment_value, team, True)
        if issue_obj:
            all_issues.append(issue_obj)

    return all_issues


def generate_backlog_issues_list(
    team: str,
    increment_value: str,
    todays_date: str, 
) -> list[BaseItem]:
    """
    Fetch raw Neo4j data for the given team/PI and build a list of BaseItem objects.

    This is the main entry point for the rest of the pipeline — every downstream
    function consumes the list this returns.
    """
    df_baseline = get_data_baseline(team, increment_value, True).filter(pl.col("status_name") == "To Do")
    issues_baseline = get_issues_list(df_baseline)

    df_actuals = get_data(team, increment_value, False, todays_date)
    issues_actuals = get_issues_list(df_actuals)

    df_actuals = df_actuals.with_columns(
        pl.col("transition_time")
            .map_elements(lambda x: x.to_native() if hasattr(x, "to_native") else x)
            .cast(pl.Datetime)
    )

    set_actuals = set(issues_actuals)
    filtered_baseline_list = [item for item in issues_baseline if item not in set_actuals]
    print(filtered_baseline_list)  

    all_issues = []
    for issue_key in issues_actuals:
        issue_obj = generate_issue_object_data(df_actuals, issue_key, increment_value, team)
        if issue_obj:
            all_issues.append(issue_obj)
    for issue_key in filtered_baseline_list: 
        issue_obj = generate_issue_object_data(df_baseline, issue_key, increment_value, team)
        if issue_obj:
            all_issues.append(issue_obj)

    return all_issues