# ---------------------------------------------------------------------------
# pipeline.py
#
# Top-level orchestrator that wires all modules together into the two
# DataFrames the dashboard needs:
#
#   dataframe_for_initial_graph() → combined_df  (actuals + baseline, daily)
#   prepare_figure_data()         → figure_df    (long format for Plotly)
#
# This is the only file a dashboard developer needs to import from.
# Allows for easier testing of sub files seen below.  
# ---------------------------------------------------------------------------

import datetime
from datetime import timedelta
import time

import polars as pl

from data_transformation_directory.Issue_Builder import generate_issues_list
from data_transformation_directory.Baseline import build_pi_baseline
from data_transformation_directory.Actuals import build_pi_actuals, _get_done_date, _get_start_date
from data_transformation_directory.projections import build_projections
from data_transformation_directory.training_projections import return_stats_df
import inspect


# ---------------------------------------------------------------------------
# Function to dynamically grab outstanding issues.  
# ---------------------------------------------------------------------------

ACTIVE_STATES = {
    "In Progress",
    "Defined", "Defining", "Analyzing",
    "In Documentation", "In Planning",
    "Verify", "Verifying", "Validate", "Validating",
    "Acceptance",
    "Architecture Review", "CSA Review",
    "Ready to Deploy", "Ready for Test",
    "Automated Testing",
    "Reopened",
    "Iteration-1", "Iteration-2", "Iteration-3", "Iteration-4", "Iteration-5",
}

'''
Active states they care about: 
    "Iteration-1", "Iteration-2", "Iteration-3", "Iteration-4", "Iteration-5"
    **Used for planning.  Part of PI Planning workflow.  
    For mercs and Thestrals: 
        *In prog, Ready to Deploy, verify, verifying, validate/validating, acceptance, and Done.  
        *Do not care about reopened.  
        *Have metrics stop at verifying.  Outside of dev hands.  
'''

EXCLUDED_STATES = {
    "Done", "Realized", "Backlog", "Inactive",
    "On Hold", "Blocked", "Triage", "Triaging", "Awaiting Approval",
}

def get_remaining_issues(all_issues, as_of_date):
    cutoff = datetime.datetime.strptime(as_of_date, "%Y-%m-%d")
    remaining = []
    
    active_sp   = 0.0
    excluded_sp = 0.0
    todo_sp     = 0.0
    done_sp     = 0.0
    excluded_breakdown = {}
    
    for item in all_issues:
        sp = float(item.weight) if item.weight else 0.0

        # Truncate transition history to only what existed as of the cutoff
        if (
            item.transitions is not None
            and not item.transitions.is_empty()
            and "start" in item.transitions.columns
        ):
            transitions_as_of = item.transitions.filter(pl.col("start") <= cutoff)
        else:
            transitions_as_of = item.transitions

        done_dt  = _get_done_date(transitions_as_of)
        start_dt = _get_start_date(transitions_as_of)

        already_done      = done_dt is not None and done_dt <= cutoff
        has_started       = start_dt is not None
        in_scope          = item.current_state not in EXCLUDED_STATES  # "Backlog", "Inactive" etc
        is_todo           = item.current_state == "To Do"

        if already_done:
            done_sp += sp
        elif has_started and in_scope:
            active_sp += sp
            remaining.append(item)
        elif is_todo:
            todo_sp += sp
            remaining.append(item)   # include unstarted in-scope work in projections
        else:
            excluded_sp += sp
            excluded_breakdown[item.current_state] = (
                excluded_breakdown.get(item.current_state, 0.0) + sp
            )

    total_sp = done_sp + active_sp + excluded_sp

    print(
        f"\n[Remaining Issues Scorecard]"
        f"\n  Done SP        : {done_sp:.1f}  ({done_sp / total_sp * 100:.1f}%)"
        f"\n  Active SP      : {active_sp:.1f}  ({active_sp / total_sp * 100:.1f}%)"
        f"\n  Excluded SP    : {excluded_sp:.1f}  ({excluded_sp / total_sp * 100:.1f}%)"
        f"\n  ─────────────────────────────"
        f"\n  Total SP       : {total_sp:.1f}  (should match baseline)"
        f"\n\n  Excluded breakdown by state:"
        + "".join(
            f"\n    {state:<25} {sp:.1f} SP"
            for state, sp in sorted(excluded_breakdown.items(), key=lambda x: -x[1])
        )
    )

    return remaining

def return_unlike_issues(all_issues_baseline, all_issues_actual):
    baseline_keys = {issue.key for issue in all_issues_baseline}
    actual_keys = {issue.key for issue in all_issues_actual}

    # In actuals but not baseline
    in_actual_only = [
        i for i in all_issues_actual
        if i.key not in baseline_keys
    ]

    # In baseline but not actuals
    in_baseline_only = [
        i for i in all_issues_baseline
        if i.key not in actual_keys
    ]

    # Issues in both but with different SP
    baseline_map = {i.key: i for i in all_issues_baseline}
    actual_map = {i.key: i for i in all_issues_actual}

    sp_mismatches = [
        (key, actual_map[key].weight, baseline_map[key].weight)
        for key in actual_keys & baseline_keys
        if actual_map[key].weight != baseline_map[key].weight
    ]

    left_lines = [
        f"In actuals only ({len(in_actual_only)} issues):"
    ]
    left_lines.extend(
        f"{i.key}  {i.current_state}  {i.weight} SP"
        for i in in_actual_only
    )

    right_lines = [
        f"In baseline only ({len(in_baseline_only)} issues):"
    ]
    right_lines.extend(
        f"{i.key}  {i.weight} SP"
        for i in in_baseline_only
    )

    # Side-by-side section
    col_width = max(len(line) for line in left_lines) + 4

    rows = []
    max_rows = max(len(left_lines), len(right_lines))
    for idx in range(max_rows):
        left = left_lines[idx] if idx < len(left_lines) else ""
        right = right_lines[idx] if idx < len(right_lines) else ""
        rows.append(f"{left:<{col_width}}{right}")

    # SP mismatch section below
    rows.append("")
    rows.append(f"SP mismatches in common issues ({len(sp_mismatches)}):")

    for key, actual_sp, baseline_sp in sp_mismatches:
        rows.append(
            f"{key}  actual={actual_sp}  baseline={baseline_sp}  "
            f"delta={actual_sp - baseline_sp}"
        )

    return "\n".join(rows)

# ---------------------------------------------------------------------------
# Core orchestrator
# ---------------------------------------------------------------------------

 
def dataframe_for_initial_graph(
    team: str,
    increment: str,
    as_of_date: str,
    just_stories: bool = True,
) -> pl.DataFrame:
    """
    Fetch issues, build the piecewise baseline and actuals series, join them
    on a continuous daily spine, and return a single combined DataFrame.
 
    Parameters
    ----------
    team         : Jira team name  (e.g. "Thestrals")
    increment    : PI label        (e.g. "PI-32")
    just_stories : if True, only Story issue types contribute to SP totals
 
    Returns
    -------
    pl.DataFrame — one row per calendar day, columns:
        date, sprint_name,
        baseline_sp, baseline_pct,
        completed_sp, completed_pct, issues_completed,
        in_progress_sp, in_progress_pct, issues_in_progress
    """
    start_time = time.time()
 
    all_issues = generate_issues_list(team, increment, just_stories, as_of_date)
    if not all_issues:
        raise ValueError(
            f"dataframe_for_initial_graph: no issues found for "
            f"team='{team}', increment='{increment}'."
        )

    all_issues_baseline = generate_issues_list(team, increment, just_stories, as_of_date, baseline = True)
    unlike_issues = return_unlike_issues(all_issues_baseline, all_issues)

    print(unlike_issues)
    
    baseline_df, baseline_string, sprint_dates = build_pi_baseline(all_issues_baseline, team)
    
    actuals_df, actuals_string, wip_dates  = build_pi_actuals(all_issues)
    remaining = get_remaining_issues(all_issues, as_of_date)

    
    # Truncate to midnight — Neo4j sprint timestamps are not always at 00:00:00
    baseline_df = baseline_df.with_columns(pl.col("date").dt.truncate("1d"))
    actuals_df  = actuals_df.with_columns(pl.col("date").dt.truncate("1d"))

    total_story_points = sum(float(issue.weight) for issue in remaining)

    remaining_issue_string = (
        "Outstanding Issues\n"
        "------------------\n"
        f"{'Key':<15} {'State':<20} {'Last Transition':<20} {'SP':>5}\n"
        f"{'-'*15} {'-'*20} {'-'*20} {'-'*5}\n"
        + "\n".join(
            f"{issue.key:<15} {issue.current_state:<20} {issue.last_transition:<20} {float(issue.weight):>5.1f}"
            for issue in remaining
        )
        + f"\n\nTotal Story Points Outstanding: {total_story_points:.1f}"
    )

    print(remaining_issue_string)

 
    # ── Full date spine ───────────────────────────────────────────────────────
    all_dates = baseline_df["date"].to_list() + actuals_df["date"].to_list()
    first_day = min(d.replace(tzinfo=None) for d in all_dates)
    last_day  = max(d.replace(tzinfo=None) for d in all_dates)
 
    spine_df = pl.DataFrame(
        [{"date": first_day + timedelta(days=i)} for i in range((last_day - first_day).days + 1)]
    ).with_columns(pl.col("date").cast(pl.Datetime("us")))
 
    # ── Join and impute ───────────────────────────────────────────────────────
    combined_df = (
        spine_df
        .join(baseline_df, on="date", how="left")
        .join(actuals_df,  on="date", how="left")
 
        # Sprint label: forward-fill across gap days (IP sprints, weekends)
        .with_columns(pl.col("sprint_name").forward_fill())
 
        # Actuals: fill nulls before the first event
        .with_columns([
            pl.col("completed_sp").forward_fill().fill_null(0.0),
            pl.col("completed_pct").forward_fill().fill_null(0.0),
            pl.col("issues_completed").forward_fill().fill_null(0).cast(pl.Int32),
            pl.col("in_progress_sp").forward_fill().fill_null(0.0),
            pl.col("in_progress_pct").forward_fill().fill_null(0.0),
            pl.col("issues_in_progress").forward_fill().fill_null(0).cast(pl.Int32),
        ])

        .with_columns([
            pl.col("baseline_sp").forward_fill().fill_null(0.0),
            pl.col("baseline_pct").forward_fill().fill_null(0.0),
        ])
 
        .select([
            "date", "sprint_name",
            "baseline_sp",    "baseline_pct",
            "completed_sp",   "completed_pct",   "issues_completed",
            "in_progress_sp", "in_progress_pct", "issues_in_progress",
        ])
        .sort("date")
    )
 
    print(
        f"[Graph] {len(combined_df)} rows | "
        f"{combined_df['date'].min().date()} → {combined_df['date'].max().date()} | "
        f"{time.time() - start_time:.2f}s"
    )

    print(f"[Graph] returning: {type(combined_df).__name__}, actuals_string type={type(actuals_string).__name__!r}, value={actuals_string!r:.80}")
    return combined_df, actuals_string, baseline_string, remaining, wip_dates, sprint_dates
 
 
# ---------------------------------------------------------------------------
# Figure data preparation
# ---------------------------------------------------------------------------
 
def prepare_figure_data(
    combined_df:    pl.DataFrame,
    projections_df: pl.DataFrame,
) -> pl.DataFrame:

    actual_completed_df = combined_df.select([
        pl.col("date").cast(pl.Date).alias("Dates"),
        pl.col("completed_pct").alias("Percent Complete"),
        pl.lit("Actual_Completed").alias("group"),
    ]).drop_nulls(["Dates", "Percent Complete"])       

    actual_in_progress_df = combined_df.select([        
        pl.col("date").cast(pl.Date).alias("Dates"),
        pl.col("in_progress_pct").alias("Percent Complete"),
        pl.lit("Actual_In_Progress").alias("group"),
    ]).drop_nulls(["Dates", "Percent Complete"])        

    baseline_df = combined_df.select([
        pl.col("date").cast(pl.Date).alias("Dates"),
        pl.col("baseline_pct").alias("Percent Complete"),
        pl.lit("Baseline").alias("group"),
    ]).drop_nulls(["Dates", "Percent Complete"])

    agg = (
        projections_df
        .group_by("date")
        .agg([
            pl.col("completed_pct").quantile(0.05).alias("p05"),
            pl.col("completed_pct").quantile(0.50).alias("median"),
            pl.col("completed_pct").quantile(0.95).alias("p95"),
        ])
        .sort("date")
    )

    def _band(col: str, group_name: str) -> pl.DataFrame:
        return agg.select([
            pl.col("date").cast(pl.Date).alias("Dates"),
            pl.col(col).alias("Percent Complete"),
            pl.lit(group_name).alias("group"),
        ])

    figure_df = pl.concat([
        actual_completed_df,                            
        actual_in_progress_df,                          
        baseline_df,
        _band("median", "projection_median"),
        _band("p05",    "projection_05"),
        _band("p95",    "projection_95"),
    ]).sort(["group", "Dates"])

    print(
        f"[FigureData] {len(figure_df)} rows | "
        f"groups: {sorted(figure_df['group'].unique().to_list())} | "
        f"{figure_df['Dates'].min()} → {figure_df['Dates'].max()}"
    )

    figure_df.write_csv("figure_df.csv")
    return figure_df
 
 
# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
 
def main():
    team         = "Thestrals"
    increment    = "PI-33"
    just_stories = True
    sprint_date  = "2026-06-12"
    team_size    = 7
    proj_count   = 1000

    function_name = inspect.currentframe().f_code.co_name
    start_time    = time.time()

    # Prior PIs used to fit the PERT duration distribution.
    # Most recent gets the highest weight at decay 0.5.
    training_increments = ["PI-33", "PI-32", "PI-31"]

    combined_df, actuals_string, baseline_string, remaining, wip_dates, sprint_dates = (
        dataframe_for_initial_graph(team, increment, sprint_date, just_stories)
    )
    total_sp = float(combined_df["baseline_sp"].max())

    training_df = return_stats_df(
        team, training_increments, just_stories=just_stories, recency_decay=0.5
    )

    projections_df, issue_dates_df = build_projections(
        combined_df,
        sprint_date,
        total_sp,
        proj_count,
        training_df=training_df,
        training_col="days_per_story_point",
        team_size = team_size,
        remaining_issues=remaining,
    )

    figure_df = prepare_figure_data(combined_df, projections_df)

    print("\n── Figure Data ───────────────────────────────")
    print(figure_df)
    print(issue_dates_df)
    
    print(f"\n{function_name}: {time.time() - start_time:.2f}s total")


if __name__ == "__main__":
    main()
 
