# =============================================================================
# callbacks.py
# =============================================================================

# === Standard Library Imports ===
import re
import datetime
import time
from typing import Iterable, List, Optional, Tuple

# === Third-Party Libraries ===
import polars as pl
from dash import ALL, Input, Output, State, ctx, no_update
from dash.exceptions import PreventUpdate

# === App-Specific Modules ===
from utils.figures import build_figure_pi_display, build_figure_scenario_comparison
from utils.alerts import build_alerts, render_alert_card, render_carryover_notice, rule_status
from utils.dropdown_utils import assemble_pi_selection_initial, assemble_pi_selection_training, assemble_dropdown_selection
from utils.restraints_check import get_earliest_and_latest_date, get_projection_start_date
from utils.sprint_plan_view import render_sprint_plan
from utils.status_view import render_status_summary
from utils.forecast_view import render_forecast_summary, render_scenario_card

# ── Pipeline ──────────────────────────────────────────────────────────────────
from data_transformation_directory.Pipeline import (
    dataframe_for_initial_graph,
    prepare_figure_data,
)
from data_transformation_directory.projections import build_projections
from data_transformation_directory.training_projections import return_stats_df

# === Neo4j credentials -- delivered via environment variables (Kubernetes
# Secret/ConfigMap injection, or exported shell vars locally). No .env file
# is read anywhere in this application. ===


# =============================================================================
# Constants
# =============================================================================

_SELECTED_STYLE = {
    "width": "100%",
    "display": "flex",
    "justifyContent": "space-between",
    "alignItems": "center",
    "gap": "12px",
    "backgroundColor": "white",
    "color": "#1F2937",
    "border": "none",
    "borderRadius": "8px",
    "padding": "10px 16px",
    "cursor": "pointer",
    "textAlign": "left",
}

_UNSELECTED_STYLE = {
    "width": "100%",
    "display": "flex",
    "justifyContent": "space-between",
    "alignItems": "center",
    "gap": "12px",
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


def _compute_completion_stats(projections_df, estimated_project_completion_date):
    """Median % complete and probability of 100% at the committed completion date."""
    # None means "no projection data on that date" (e.g. the committed date
    # falls before the projection start), which the view shows as "—".
    completion_percentage = None
    likelihood_percentage = None

    if not estimated_project_completion_date:
        return completion_percentage, likelihood_percentage

    try:
        comp_date = datetime.datetime.strptime(
            str(estimated_project_completion_date)[:10], "%Y-%m-%d"
        ).date()

        proj_at_comp = projections_df.filter(pl.col("date").cast(pl.Date) == comp_date)

        if not proj_at_comp.is_empty():
            completion_percentage = round(float(proj_at_comp["completed_pct"].median()), 2)
            likelihood_percentage = round(
                float((proj_at_comp["completed_pct"] >= 99.9999).mean() * 100), 4
            )
    except Exception as e:
        print(f"[Simulation] stat computation failed: {e}")

    return completion_percentage, likelihood_percentage


def _median_peak(figure_df):
    """Highest % the median projection reaches, or None."""
    rows = figure_df.filter(pl.col("group") == "projection_median")
    return float(rows["Percent Complete"].max()) if rows.height else None


def _forecast_dict(completion_pct, likelihood_pct, date_value, target_date, figure_df, projection_count):
    """Everything render_forecast_summary needs, JSON-safe."""
    return {
        "likelihood_pct":   likelihood_pct,
        "completion_pct":   completion_pct,
        "finish_date":      str(date_value) if date_value is not None else None,
        "target_date":      str(target_date)[:10] if target_date else None,
        "median_peak":      _median_peak(figure_df),
        "projection_count": int(projection_count),
    }


def _figure_df_to_store(figure_df: pl.DataFrame) -> dict:
    """figure_df → JSON-safe dict for a dcc.Store."""
    return figure_df.with_columns(pl.col("Dates").cast(pl.Utf8)).to_dict(as_series=False)


def _figure_df_from_store(data: dict) -> pl.DataFrame:
    """
    Rebuild figure_df from a dcc.Store. The store round-trips through the
    browser, and JavaScript has one number type, so 0.0 comes back as 0.
    Pin the column types instead of letting polars guess from the first rows.
    """
    return (
        pl.DataFrame(
            data,
            schema_overrides={"Dates": pl.Utf8, "Percent Complete": pl.Float64, "group": pl.Utf8},
            strict=False,
        )
        .with_columns(pl.col("Dates").str.to_date())
    )


def _scenario_params(todays_date, team, main_increment, training_increments,
                     estimated_project_completion_date, team_size, issue_option) -> dict:
    """Inputs that define a scenario run; used to tell whether a stored run is still current."""
    return {
        "todays_date":  str(todays_date),
        "team":         team,
        "increment":    main_increment,
        "training":     sorted(training_increments or []),
        "target":       str(estimated_project_completion_date),
        "team_size":    team_size,
        "issue_option": issue_option,
    }


def _median_completion_date(figure_df):
    """First date the median projection reaches 100%, or None."""
    median_complete = (
        figure_df
        .filter(
            (pl.col("group") == "projection_median") &
            (pl.col("Percent Complete") >= 100.0)
        )
        .sort("Dates")
        .head(1)
    )
    return median_complete["Dates"][0] if not median_complete.is_empty() else None


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
) -> Optional[dict]:
    """
    Run the projection simulation for Tab 1 / Tab 2.

    Returns a dict with keys:
        plot_figure, alerts, date_value,
        completion_pct, likelihood_pct, actual_string, sprint_plan, status_summary
    or None if any step produces no data.

    Pipeline
    --------
    1. dataframe_for_initial_graph  — fetch issues, build baseline + actuals
    2. return_stats_df              — fit PERT velocity from weighted history
    3. build_projections            — run N Monte-Carlo paths from todays_date
    4. compute stats                — median % and likelihood at comp date
    5. prepare_figure_data          — collapse runs → p05 / median / p95 bands
    6. build_figure_pi_display      — render the Plotly figures
    """
    just_stories = (issue_option == "Just Stories")

    # ── 1. Build combined actuals + piecewise baseline ────────────────────────
    try:
        combined_df, actual_string, sprint_plan, remaining, wip_dates, sprint_dates, status_summary = (
            dataframe_for_initial_graph(team, main_increment, todays_date, just_stories)
        )
    except Exception:
        import traceback
        traceback.print_exc()
        return None

    total_sp = float(combined_df["baseline_sp"].max())
    if total_sp == 0:
        print("[Simulation] total_sp is 0 — no story points found.")
        return None

    # ── 2. Weighted PERT velocity from historical training increments ─────────
    # recency_decay=0.5 → most-recent PI weight 1.0, each prior PI * 0.5.
    if not training_increments:
        print("[Simulation] no training increments selected/available — no data.")
        return None

    training_df = return_stats_df(team, training_increments, just_stories, recency_decay=recency_decay)

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
        return None

    # ── 4. Completion % and likelihood at the target date ─────────────────────
    completion_pct, likelihood_pct = _compute_completion_stats(
        projections_df, estimated_project_completion_date
    )

    # ── 5. Collapse N runs → p05 / median / p95 bands ─────────────────────────
    figure_df = prepare_figure_data(combined_df, projections_df)

    # ── 6. When the median projection first reaches 100 % ─────────────────────
    date_value     = _median_completion_date(figure_df)
    proj_comp_date = str(date_value) if date_value is not None else None

    # ── 7. Render the Plotly figures ──────────────────────────────────────────
    plot_figure = build_figure_pi_display(figure_df, todays_date, proj_comp_date)

    # ── 8. Alert rules for the Realtime Alerts tab ────────────────────────────
    alerts = build_alerts(combined_df, sprint_plan, todays_date)

    return {
        "plot_figure":    _apply_dark_text(plot_figure),
        "figure_df":      figure_df,
        "alerts":         alerts,
        "date_value":     date_value,
        "completion_pct": completion_pct,
        "likelihood_pct": likelihood_pct,
        "actual_string":  actual_string,
        "sprint_plan":    sprint_plan,
        "status_summary": status_summary,
        "forecast":       _forecast_dict(completion_pct, likelihood_pct, date_value,
                                         estimated_project_completion_date, figure_df, projection_count),
    }


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
) -> Optional[dict]:
    """
    Run one projection scenario for Tab 3 (data only; the comparison chart is
    drawn by the callback once both scenarios are available).

    Returns a dict with keys:
        figure_df, date_value, completion_pct, likelihood_pct, forecast
    or None if any step produces no data.
    """
    just_stories = (issue_option == "Just Stories")

    # ── 1. Build combined actuals + piecewise baseline ────────────────────────
    try:
        combined_df, _, _, remaining, _, _, _ = dataframe_for_initial_graph(
            team, main_increment, todays_date, just_stories
        )
    except Exception:
        import traceback
        traceback.print_exc()
        return None

    total_sp = float(combined_df["baseline_sp"].max())
    if total_sp == 0:
        print("[Simulation] total_sp is 0 — no story points found.")
        return None

    # ── 2. Weighted PERT velocity from historical training increments ─────────
    if not training_increments:
        print("[Simulation] no training increments selected/available — no data.")
        return None

    training_df = return_stats_df(team, training_increments, just_stories, recency_decay=recency_decay)

    # ── 3. Run N projection paths branching from todays_date ──────────────────
    try:
        projections_df, issue_dates_df = build_projections(
            combined_df      = combined_df,
            sprint_date      = todays_date,
            total_sp         = total_sp,
            projection_count = int(projection_count),
            training_df      = training_df,
            team_size        = team_size,
            remaining_issues = remaining,
        )
    except ValueError as e:
        print(f"[Simulation] build_projections failed: {e}")
        return None

    # ── 4–6. Stats, bands, median completion date ─────────────────────────────
    completion_pct, likelihood_pct = _compute_completion_stats(
        projections_df, estimated_project_completion_date
    )
    figure_df  = prepare_figure_data(combined_df, projections_df)
    date_value = _median_completion_date(figure_df)

    return {
        "figure_df":      figure_df,
        "date_value":     date_value,
        "completion_pct": completion_pct,
        "likelihood_pct": likelihood_pct,
        "forecast":       _forecast_dict(completion_pct, likelihood_pct, date_value,
                                         estimated_project_completion_date, figure_df, projection_count),
    }


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
        State("issue_type_selection", "value"),
    )
    def autofill_pi_dates(selected_pi, team, just_stories):
        if not team or not selected_pi or not just_stories:
            raise PreventUpdate
        start_date, end_date = get_earliest_and_latest_date(team, selected_pi, just_stories)
        return str(start_date), str(end_date)

    @app.callback(
        Output("todays-date", "date"),
        Input("pi-display-dropdown", "value"),
        State("team-dropdown", "value"),
        State("issue_type_selection", "value"),
    )
    def autofill_projection_start(selected_pi, team, just_stories):
        if not team or not selected_pi or not just_stories:
            raise PreventUpdate
        projection_start = get_projection_start_date(team, selected_pi, just_stories)
        return str(projection_start)

    @app.callback(
        Output("progress-graph-projection", "figure"),
        Output("alerts-store", "data"),
        Output("scenario1-store", "data"),
        Output("forecast-summary", "children"),
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

        result = _run_simulation_initial(
            todays_date, team, main_increment, training_increments,
            estimated_project_completion_date,
            team_size, issue_option,
        )

        if result is None:
            raise PreventUpdate

        print(f"update_projection_parameters took {time.time() - start_time:.2f}s")
        # Keep this run so the Team Impact tab can reuse it as Scenario 1
        scenario1 = {
            "params":    _scenario_params(todays_date, team, main_increment, training_increments,
                                          estimated_project_completion_date, team_size, issue_option),
            "figure_df": _figure_df_to_store(result["figure_df"]),
            "forecast":  result["forecast"],
        }

        return (
            result["plot_figure"],
            result["alerts"],
            scenario1,
            render_forecast_summary(result["forecast"]),
            render_status_summary(result["status_summary"]),
            render_sprint_plan(result["sprint_plan"], todays_date),
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
        triggered    = ctx.triggered_id
        new_selected = triggered["index"] if triggered else current_selected
        styles = [
            _SELECTED_STYLE if i == new_selected else _UNSELECTED_STYLE
            for i in range(len(n_clicks))
        ]
        return styles, new_selected

    @app.callback(
        Output("progress-graph-projection-tab2", "figure"),
        Output("alert-card", "children"),
        Output("alert-card", "style"),
        Output("rule-status-wip", "children"),
        Output("rule-status-sprint", "children"),
        Output("carryover-notice", "children"),
        Output("carryover-notice", "style"),
        Input("selected-option", "data"),
        Input("alerts-store", "data"),
    )
    def render_selected_alert(selected_index, alerts):
        alerts   = alerts or {}
        selected = alerts.get("wip" if selected_index == 0 else "sprint")
        card, style = render_alert_card(selected)
        figure = selected["figure"] if selected else {}
        notice, notice_style = render_carryover_notice(alerts.get("increments"))
        return (
            figure, card, style,
            rule_status(alerts.get("wip")),
            rule_status(alerts.get("sprint")),
            notice, notice_style,
        )

    # -------------------------------------------------------------------------
    # Tab 3: Team Impact Adjustment
    # -------------------------------------------------------------------------

    @app.callback(
        Output("progress-graph-projection-tab3", "figure"),
        Output("forecast-summary-tab3-team1", "children"),
        Output("forecast-summary-tab3-team2", "children"),
        Input("generate-graph-projections-btn-tab3", "n_clicks"),
        State("todays-date", "date"),
        State("team-dropdown", "value"),
        State("pi-display-dropdown", "value"),
        State("pi-training-dropdown", "value"),
        State("estimated-project-completion-date", "date"),
        State("team_size", "value"),
        State("team_size-tab3", "value"),
        State("issue_type_selection", "value"),
        State("scenario1-store", "data"),
        prevent_initial_call=True,
    )
    def update_scenario_comparison(
        n_clicks, todays_date, team, main_increment, training_increments,
        estimated_project_completion_date,
        team_size_1, team_size_2, issue_option, scenario1_store,
    ):
        start_time = time.time()
        common = (todays_date, team, main_increment, training_increments,
                  estimated_project_completion_date)

        # ── Scenario 1: reuse the Projection Chart run if its inputs still match
        params_1 = _scenario_params(*common, team_size_1, issue_option)
        if scenario1_store and scenario1_store.get("params") == params_1:
            figure_df_1 = _figure_df_from_store(scenario1_store["figure_df"])
            forecast_1  = scenario1_store["forecast"]
        else:
            run_1 = _run_simulation(*common, team_size_1, issue_option)
            if run_1 is None:
                raise PreventUpdate
            figure_df_1, forecast_1 = run_1["figure_df"], run_1["forecast"]

        # ── Scenario 2: the adjusted team size
        run_2 = _run_simulation(*common, team_size_2, issue_option)
        if run_2 is None:
            raise PreventUpdate

        label_1 = f"Scenario 1: {team_size_1} people"
        label_2 = f"Scenario 2: {team_size_2} people"

        # Not passed through _apply_dark_text: that recolors every annotation
        # black, and the end-of-line labels need their scenario colors.
        figure = build_figure_scenario_comparison(
            [
                {"label": label_1, "figure_df": figure_df_1},
                {"label": label_2, "figure_df": run_2["figure_df"]},
            ],
            todays_date,
        )

        print(f"update_scenario_comparison took {time.time() - start_time:.2f}s")
        return (
            figure,
            render_scenario_card(0, label_1, forecast_1),
            render_scenario_card(1, label_2, run_2["forecast"]),
        )
