# =============================================================================
# callbacks.py
# =============================================================================

# === Standard Library Imports ===
import os
import re
import datetime
from typing import Iterable, List, Dict, Optional, Tuple
from collections.abc import Iterable

# === Third-Party Libraries ===
import polars as pl
import dash
from dash import ALL, Input, Output, State, ctx, no_update
import time

# === App-Specific Modules ===
from utils.figures import build_figure_pi_display, build_figure_pi_display_initial
from utils.dropdown_utils import assemble_pi_selection_initial, assemble_pi_selection_training, assemble_dropdown_selection
from utils.restraints_check import get_earliest_and_latest_date, get_projection_start_date

# ── New pipeline (replaces team_projections + projections_beta calls) ─────────
from data_transformation_directory.Pipeline import (
    dataframe_for_initial_graph,
    prepare_figure_data,
)
from data_transformation_directory.projections import build_projections
from data_transformation_directory.training_projections import return_stats_df

# ── Still needed for dropdown helpers ─────────────────────────────────────────
from data_transformation_directory.WorkItem.jira_map import IN_PROGRESS_STATE
from data_transformation_directory.WorkItem.helper import (
    get_work_hours,
    is_late_story,
)

# === Neo4j credentials -- delivered via environment variables (Kubernetes
# Secret/ConfigMap injection, or exported shell vars locally). No .env file
# is read anywhere in this application. ===



# =============================================================================
# Constants
# =============================================================================

_SELECTED_STYLE = {
    "backgroundColor": "white",
    "color": "#1F2937",
    "border": "none",
    "borderRadius": "8px",
    "padding": "10px 16px",
    "cursor": "pointer",
    "textAlign": "left",
}

_UNSELECTED_STYLE = {
    "backgroundColor": "transparent",
    "color": "#6B7280",
    "border": "none",
    "borderRadius": "8px",
    "padding": "10px 16px",
    "cursor": "pointer",
    "textAlign": "left",
}


# =============================================================================
# Module-level helper functions
# =============================================================================

def _apply_dark_text(fig):
    fig.update_layout(
        template="plotly_white",
        font=dict(color="#000000"),
        title_font=dict(color="#000000"),
        paper_bgcolor="rgba(0,0,0,0)",
        plot_bgcolor="rgba(0,0,0,0)",
        legend=dict(font=dict(color="#000000")),
        legend_title=dict(font=dict(color="#000000")),
        hoverlabel=dict(bgcolor="rgba(255,255,255,0.9)", font=dict(color="#000000")),
        coloraxis_colorbar=dict(title_font_color="#000000", tickfont_color="#000000"),
    )
    fig.update_xaxes(title_font=dict(color="#000000"), tickfont=dict(color="#000000"))
    fig.update_yaxes(title_font=dict(color="#000000"), tickfont=dict(color="#000000"))
    fig.update_traces(textfont=dict(color="#000000"))
    fig.for_each_annotation(lambda a: a.update(font=dict(color="#000000")))

    if hasattr(fig.layout, "scene"):
        fig.update_scenes(
            xaxis=dict(title_font=dict(color="#000000"), tickfont=dict(color="#000000")),
            yaxis=dict(title_font=dict(color="#000000"), tickfont=dict(color="#000000")),
            zaxis=dict(title_font=dict(color="#000000"), tickfont=dict(color="#000000")),
        )
    return fig

def _run_simulation_initial(
    todays_date:                       str,
    team:                              str,
    main_increment:                    str,
    training_increments:               List[str],
    estimated_project_completion_date: str,
    team_size:                         int,
    issue_option:                      str,
    projection_count:                  int   = 1000,
    recency_decay:                     float = 0.5,
) -> Tuple[Optional[object], Optional[object], float, float]:
    """
    Run the projection simulation using the new pipeline and return
    (plot_figure, date_value, completion_pct, likelihood_pct).

    Returns (None, None, 0.0, 0.0) if any step produces no data.

    Pipeline
    --------
    1. dataframe_for_initial_graph  — fetch issues, build baseline + actuals
    2. return_stats_df              — fit PERT velocity from weighted history
    3. build_projections            — run N Monte-Carlo paths from todays_date
    4. compute stats                — median % and likelihood at comp date
    5. prepare_figure_data          — collapse runs → p05 / median / p95 bands
    6. build_figure_pi_display      — render the Plotly figure
    """
    just_stories = (issue_option == "Just Stories")

    # ── 1. Build combined actuals + piecewise baseline ────────────────────────
    try:
        combined_df, actual_string, baseline_string, remaining, wip_dates, sprint_dates = dataframe_for_initial_graph(
            team, main_increment, todays_date, just_stories
        )
    except Exception as e:
        import traceback
        traceback.print_exc()
        return None, None, 0.0, 0.0, None, None

    total_sp = float(combined_df["baseline_sp"].max())
    if total_sp == 0:
        print("[Simulation] total_sp is 0 — no story points found.")
        return None, None, 0.0, 0.0, None, None

    # ── 2. Weighted PERT velocity from historical training increments ──────────
    #
    # recency_decay=0.5 → most-recent PI weight 1.0, each prior PI * 0.5.
    # Set recency_decay=0.0 to treat all training PIs equally.


    training_df = return_stats_df(
        team,
        training_increments,
        just_stories,
        recency_decay=recency_decay,
    )

    # ── 3. Run N projection paths branching from todays_date ──────────────────
    try:
        projections_df, issue_dates_df = build_projections(
            combined_df      = combined_df,
            sprint_date      = todays_date,     # projection branch point
            total_sp         = total_sp,
            projection_count = int(projection_count),
            training_df      = training_df,
            team_size        = team_size, 
            remaining_issues = remaining, 
        )
    except ValueError as e:
        print(f"[Simulation] build_projections failed: {e}")
        return None, None, 0.0, 0.0, None, None

    # ── 4. Compute completion % and likelihood at the target date ─────────────
    completion_percentage = 0.0
    likelihood_percentage = 0.0

    if estimated_project_completion_date:
        try:
            comp_date = datetime.datetime.strptime(
                estimated_project_completion_date, "%Y-%m-%d"
            ).date()

            proj_at_comp = projections_df.filter(
                pl.col("date").cast(pl.Date) == comp_date
            )

            if not proj_at_comp.is_empty():
                completion_percentage = round(
                    float(proj_at_comp["completed_pct"].median()), 2
                )
                likelihood_percentage = round(
                    float(
                        (proj_at_comp["completed_pct"] >= 99.9999).mean() * 100
                    ),
                    4,
                )
        except Exception as e:
            print(f"[Simulation] stat computation failed: {e}")

    # ── 5. Collapse N runs → p05 / median / p95 bands for the figure ──────────
    #
    # prepare_figure_data produces the long-format DataFrame with columns
    # [Dates, Percent Complete, group] that build_figure_pi_display expects.
    figure_df = prepare_figure_data(combined_df, projections_df)

    # ── 6. Find when the median projection first reaches 100 % ────────────────
    median_complete = (
        figure_df
        .filter(
            (pl.col("group") == "projection_median") &
            (pl.col("Percent Complete") >= 100.0)
        )
        .sort("Dates")
        .head(1)
    )
    date_value = (
        median_complete["Dates"][0]
        if not median_complete.is_empty()
        else None
    )

    # ── 7. Render the Plotly figure ───────────────────────────────────────────
    proj_comp_date = (
        str(date_value)
        if date_value is not None
        else None
    )

    plot_figure, wip_figure, sprint_figure = build_figure_pi_display_initial(figure_df, todays_date, proj_comp_date, wip_dates, sprint_dates)

    plot_figure   = _apply_dark_text(plot_figure)
    wip_figure    = _apply_dark_text(wip_figure)
    sprint_figure = _apply_dark_text(sprint_figure)
 
    return plot_figure, wip_figure, sprint_figure, date_value, completion_percentage, likelihood_percentage, actual_string, baseline_string, 


def _run_simulation(
    todays_date:                       str,
    team:                              str,
    main_increment:                    str,
    training_increments:               List[str],
    estimated_project_completion_date: str,
    team_size:                         int,
    issue_option:                      str,
    projection_count:                  int   = 1000,
    recency_decay:                     float = 0.5,
) -> Tuple[Optional[object], Optional[object], float, float]:
    """
    Run the projection simulation using the new pipeline and return
    (plot_figure, date_value, completion_pct, likelihood_pct).

    Returns (None, None, 0.0, 0.0) if any step produces no data.

    Pipeline
    --------
    1. dataframe_for_initial_graph  — fetch issues, build baseline + actuals
    2. return_stats_df              — fit PERT velocity from weighted history
    3. build_projections            — run N Monte-Carlo paths from todays_date
    4. compute stats                — median % and likelihood at comp date
    5. prepare_figure_data          — collapse runs → p05 / median / p95 bands
    6. build_figure_pi_display      — render the Plotly figure
    """
    just_stories = (issue_option == "Just Stories")

    # ── 1. Build combined actuals + piecewise baseline ────────────────────────
    try:
        combined_df, actual_string, baseline_string, remaining, _, _ = dataframe_for_initial_graph(
            team, main_increment, todays_date, just_stories
        )
    except Exception as e:
        import traceback
        traceback.print_exc()
        return None, None, 0.0, 0.0, None, None

    total_sp = float(combined_df["baseline_sp"].max())
    if total_sp == 0:
        print("[Simulation] total_sp is 0 — no story points found.")
        return None, None, 0.0, 0.0, None, None

    # ── 2. Weighted PERT velocity from historical training increments ──────────
    #
    # recency_decay=0.5 → most-recent PI weight 1.0, each prior PI * 0.5.
    # Set recency_decay=0.0 to treat all training PIs equally.


    training_df = return_stats_df(
        team,
        training_increments,
        just_stories,
        recency_decay=recency_decay,
    )

    # ── 3. Run N projection paths branching from todays_date ──────────────────
    try:
        projections_df, issue_dates_df = build_projections(
            combined_df      = combined_df,
            sprint_date      = todays_date,     # projection branch point
            total_sp         = total_sp,
            projection_count = int(projection_count),
            training_df      = training_df,
            team_size        = team_size, 
            remaining_issues = remaining, 
        )
    except ValueError as e:
        print(f"[Simulation] build_projections failed: {e}")
        return None, None, 0.0, 0.0, None, None

    # ── 4. Compute completion % and likelihood at the target date ─────────────
    completion_percentage = 0.0
    likelihood_percentage = 0.0

    if estimated_project_completion_date:
        try:
            comp_date = datetime.datetime.strptime(
                estimated_project_completion_date, "%Y-%m-%d"
            ).date()

            proj_at_comp = projections_df.filter(
                pl.col("date").cast(pl.Date) == comp_date
            )

            if not proj_at_comp.is_empty():
                completion_percentage = round(
                    float(proj_at_comp["completed_pct"].median()), 2
                )
                likelihood_percentage = round(
                    float(
                        (proj_at_comp["completed_pct"] >= 99.9999).mean() * 100
                    ),
                    4,
                )
        except Exception as e:
            print(f"[Simulation] stat computation failed: {e}")

    # ── 5. Collapse N runs → p05 / median / p95 bands for the figure ──────────
    #
    # prepare_figure_data produces the long-format DataFrame with columns
    # [Dates, Percent Complete, group] that build_figure_pi_display expects.
    figure_df = prepare_figure_data(combined_df, projections_df)

    # ── 6. Find when the median projection first reaches 100 % ────────────────
    median_complete = (
        figure_df
        .filter(
            (pl.col("group") == "projection_median") &
            (pl.col("Percent Complete") >= 100.0)
        )
        .sort("Dates")
        .head(1)
    )
    date_value = (
        median_complete["Dates"][0]
        if not median_complete.is_empty()
        else None
    )

    # ── 7. Render the Plotly figure ───────────────────────────────────────────
    proj_comp_date = (
        str(date_value)
        if date_value is not None
        else None
    )

    plot_figure = _apply_dark_text(
        build_figure_pi_display(figure_df, todays_date, proj_comp_date)
    )

    return plot_figure, date_value, completion_percentage, likelihood_percentage, actual_string, baseline_string, 


# ---------------------------------------------------------------------------
# Helpers unchanged from the original
# ---------------------------------------------------------------------------

def return_training_increments(
    selected_pi: str,
    opts: Iterable[str],
    k: int = 2,
    verbose: bool = True,
) -> List[str]:
    """Pull the k most recent PIs prior to selected_pi for PERT training."""

    m = re.fullmatch(r"(.*?)-(\d+)", selected_pi.strip())
    if not m:
        if verbose:
            print("No increments available for training")
        return []

    prefix, sel_num = m.group(1), int(m.group(2))
    prefix_hyphen = f"{prefix}-"

    # ---- normalize opts into ints ----
    available_nums = set()
    for v in opts:
        v = v.strip()
        mv = re.fullmatch(rf"{re.escape(prefix_hyphen)}(\d+)", v)
        if mv:
            available_nums.add(int(mv.group(1)))

    # ---- walk backwards ----
    out = []
    n = sel_num - 1

    while len(out) < k and n >= 0:
        if n in available_nums:
            out.append(f"{prefix_hyphen}{n}")

        # stop early if we've gone below dataset range
        if available_nums and n < min(available_nums):
            break

        n -= 1

    if not out and verbose:
        print("No increments available for training")

    return out


def _normalize_teams(selected):
    if selected is None:
        return []
    if isinstance(selected, (list, tuple, set)):
        return [t for t in selected if t is not None]
    return [selected]


def _num_after_last_hyphen(text) -> float:
    s = str(text).split("-")[-1]
    m = re.search(r"[-+]?\d*\.?\d+", s)
    return float(m.group(0)) if m else float("-inf")


def _sorted_pi_options(teams):
    opts = assemble_pi_selection_initial(teams)
    top_value = opts[2] if len(opts) > 5 else None
    return opts, top_value


# =============================================================================
# Callback registration
# =============================================================================

def register_callbacks(app):

    # -------------------------------------------------------------------------
    # Shared / App-level
    # -------------------------------------------------------------------------

    @app.callback(
        Output("team-dropdown", "options"),
        Input("team-refresh-interval", "n_intervals"),
    )
    def refresh_team_options(_n_intervals):
        try:
            teams = assemble_dropdown_selection()
        except Exception as e:
            print(f"WARN: failed to refresh team list: {e}")
            return no_update
        return [{"label": t, "value": t} for t in teams]

    @app.callback(
        Output("advanced-collapse", "is_open"),
        Input("advanced-button", "n_clicks"),
        State("advanced-collapse", "is_open"),
    )
    def toggle_collapse(n, is_open):
        if n:
            return not is_open
        return is_open

    # -------------------------------------------------------------------------
    # Tab 1: Projection
    # -------------------------------------------------------------------------

    @app.callback(
        Output("pi-display-dropdown", "options"),
        Output("pi-display-dropdown", "value"),
        Output("pi-display-dropdown", "disabled"),
        Input("team-dropdown", "value"),
        State("pi-display-dropdown", "value"),
    )
    def change_available_pis_for_initial(selected_teams, current_pi):
        teams = _normalize_teams(selected_teams)
        if not teams:
            return [], None, True
        opts_sorted, top_value = _sorted_pi_options(teams)
        return opts_sorted, top_value, False

    @app.callback(
        Output("pi-training-dropdown", "options"),
        Output("pi-training-dropdown", "value"),
        Output("pi-training-dropdown", "disabled"),
        Input("team-dropdown", "value"),
        Input("pi-display-dropdown", "value"),
        State("pi-training-dropdown", "value"),
        prevent_initial_call=False,
    )
    def change_available_pis_for_training(selected_team, selected_pi, current_value):
        if not selected_team or not selected_pi:
            return [], None, True
        opts      = assemble_pi_selection_training(selected_team, selected_pi)
        new_value = return_training_increments(selected_pi, opts)
        return opts, new_value, (len(opts) == 0)

    @app.callback(
        Output("sprint-startdate-input", "date"),
        Output("estimated-project-completion-date", "date"),
        Input("pi-display-dropdown", "value"),
        State("team-dropdown", "value"),
        State("issue_type_selection", "value")
    )
    def autofill_start_date(selected_pi, team, just_stories):
        if not team or not selected_pi or not just_stories:
            return no_update, no_update
        start_date, end_date = get_earliest_and_latest_date(team, selected_pi, just_stories)
        return str(start_date), str(end_date)
        
    @app.callback(
        Output("todays-date", "date"),
        Input("pi-display-dropdown", "value"),
        State("team-dropdown", "value"),
        State("issue_type_selection", "value")
    )
    def autofill_start_date(selected_pi, team, just_stories):
        if not team or not selected_pi or not just_stories:
            return no_update, no_update
        projection_start = get_projection_start_date(team, selected_pi, just_stories)
        return str(projection_start)

    @app.callback(
        Output("progress-graph-projection", "figure"),
        Output("wip-figure-store", "data"),
        Output("sprint-figure-store", "data"),
        Output("progress-graph-projection-tab3-team1", "figure"),
        Output("completion-date", "children"),
        Output("completion-date-team1-tab3", "children"),
        Output("projected-projection-completion", "children"),
        Output("projected-projection-completion-team1-tab3", "children"),
        Output("probability_finishing_by_completion_date", "children"),
        Output("probability_finishing_by_completion_date-team1-tab3", "children"),
        Output("remaining_issue_display", "children"),
        Output("sprint_plan_display_string", "children"),
        Input("generate-graph-projections-btn", "n_clicks"),
        State("todays-date", "date"),
        State("team-dropdown", "value"),
        State("pi-display-dropdown", "value"),
        State("pi-training-dropdown", "value"),
        State("estimated-project-completion-date", "date"),
        State("team_size", "value"),
        State("issue_type_selection", "value"),
        prevent_initial_call=True,
    )
    def update_projection_parameters(
        n_clicks, todays_date, team, main_increment, training_increments,
        estimated_project_completion_date,
        team_size, issue_option,
    ):
        start_time = time.time()

        plot_figure, wip_figure, sprint_figure, date_value, completion_pct, likelihood_pct, actual_string, baseline_string = _run_simulation_initial(
            todays_date, team, main_increment, training_increments,
            estimated_project_completion_date,
            team_size, issue_option
        )

        if plot_figure is None:
            return no_update

        print(f"update_projection_parameters took {time.time() - start_time:.2f}s")
        return (
            plot_figure, wip_figure, sprint_figure, plot_figure,
            date_value, date_value,
            completion_pct, completion_pct,
            likelihood_pct, likelihood_pct, actual_string, baseline_string
        )

    # -------------------------------------------------------------------------
    # Tab 2: Realtime Alerts
    # -------------------------------------------------------------------------

    @app.callback(
        Output({"type": "v-option", "index": ALL}, "style"),
        Output("selected-option", "data"),
        Input({"type": "v-option", "index": ALL}, "n_clicks"),
        State("selected-option", "data"),
        prevent_initial_call=True,
    )
    def update_selection(n_clicks, current_selected):
        triggered   = ctx.triggered_id
        new_selected = triggered["index"] if triggered else current_selected
        styles = [
            _SELECTED_STYLE if i == new_selected else _UNSELECTED_STYLE
            for i in range(len(n_clicks))
        ]
        return styles, new_selected
    
    @app.callback(
    Output("progress-graph-projection-tab2", "figure"),
    Input("selected-option", "data"),
    Input("wip-figure-store", "data"),
    Input("sprint-figure-store", "data"),
    )
    def render_selected_figure(selected_index, wip_fig, sprint_fig):
        if selected_index == 0:
            return wip_fig or {}
        return sprint_fig or {}

    # -------------------------------------------------------------------------
    # Tab 3: Team Impact Adjustment
    # -------------------------------------------------------------------------

    @app.callback(
        Output("progress-graph-projection-tab3-team2", "figure"),
        Output("completion-date-team2-tab3", "children"),
        Output("projected-projection-completion-team2-tab3", "children"),
        Output("probability_finishing_by_completion_date-team2-tab3", "children"),
        Input("generate-graph-projections-btn-tab3", "n_clicks"),
        State("todays-date", "date"),
        State("team-dropdown", "value"),
        State("pi-display-dropdown", "value"),
        State("pi-training-dropdown", "value"),
        State("estimated-project-completion-date", "date"),
        State("team_size-tab3", "value"),
        State("issue_type_selection", "value"),
        prevent_initial_call=True,
    )
    def update_projection_parameters_tab3(
        n_clicks, todays_date, team, main_increment, training_increments,
        estimated_project_completion_date,
        team_size, issue_option,
    ):
        start_time = time.time()

        plot_figure, date_value, completion_pct, likelihood_pct, _, _= _run_simulation(
            todays_date, team, main_increment, training_increments,
            estimated_project_completion_date,
             team_size, issue_option,
        )

        if plot_figure is None:
            return no_update

        print(f"update_projection_parameters_tab3 took {time.time() - start_time:.2f}s")
        return plot_figure, date_value, completion_pct, likelihood_pct
