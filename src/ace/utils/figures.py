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

import plotly.graph_objects as go
import polars as pl
import numpy as np
from datetime import date as _date, datetime as _datetime, timedelta as _td
import time
import inspect


# =============================================================================
# Shared constants
# =============================================================================

PROJECTION_GROUPS = frozenset({"projection_median", "projection_05", "projection_95"})
SKIP_GROUPS       = frozenset({"projection_05", "projection_95", "SprintStep"})
ACTUAL_GROUPS     = ("Actual_Completed", "Actual_In_Progress")

# Series held flat until the lower projection band reaches its peak.
# Add "Baseline" here to keep the plan line at 100% alongside them.
EXTEND_GROUPS     = ACTUAL_GROUPS

COLORS = {
    "Actual":               "#1D3557",
    "Actual_Completed":     "#1D3557",
    "Actual_In_Progress":   "#4F7CAC",
    "Baseline":             "#D16D6A",
    "projection_median":    "#6B7280",
    "band":                 "rgba(100, 116, 139, 0.16)",
    "today_line":           "#94A3B8",
    "comp_line":            "#475569",
    "pi_beg_line":          "#1A3B55",
    "pi_end_line":          "#7C3AED",
    "plateau_line":         "#B45309",
    "grid":                 "rgba(148, 163, 184, 0.16)",
    "bg":                   "#FAFAF9",
    "paper":                "#FAFAF9",
}

DISPLAY_NAMES = {
    "Actual":               "Actual",
    "Actual_Completed":     "Actual Done",
    "Actual_In_Progress":   "Actual In Progress",
    "Baseline":             "Baseline",
    "projection_median":    "Median projection",
}

# How far past each "end" event the x-axis extends
PAD_AFTER_PI_END      = _td(weeks=2)
PAD_AFTER_PROJ_END    = _td(weeks=2)
PAD_AFTER_LAST_ACTUAL = _td(weeks=2)

# When the projected-maximum line is the last event on the chart, the axis
# stops just after it (room for the label) instead of padding by weeks.
PAD_AFTER_PLATEAU     = _td(days=3)

# The median counts as "reaching 100%" at or above this fraction
COMPLETE_THRESHOLD    = 0.999999

_TOL = 1e-9

# Scenario comparison (Tab 3): one color + dash style per scenario, so the two
# projections stay distinguishable in color and in grayscale.
SCENARIO_STYLES = [
    {"color": "#0F766E", "band": "rgba(15, 118, 110, 0.15)", "dash": "dot"},    # Scenario 1: teal
    {"color": "#C2410C", "band": "rgba(194, 65, 12, 0.15)",  "dash": "dash"},   # Scenario 2: orange
]


# =============================================================================
# Helpers
# =============================================================================

def _to_date(value) -> _date | None:
    """Accept str / date / datetime / None and return a date (or None)."""
    if value is None or value == "":
        return None
    if isinstance(value, _datetime):      # must precede the date check
        return value.date()
    if isinstance(value, _date):
        return value
    return _date.fromisoformat(str(value)[:10])


def _to_dates(values) -> list[_date]:
    out = []
    for v in values or []:
        d = _to_date(v)
        if d is not None:
            out.append(d)
    return out


def _first_date_at_max(gdf: pl.DataFrame | None) -> _date | None:
    """First date a series reaches its own maximum."""
    if gdf is None or gdf.height == 0:
        return None
    peak = gdf["Percent Complete"].max()
    return gdf.filter(pl.col("Percent Complete") >= peak - _TOL)["Dates"].min()


def _last_change_date(gdf: pl.DataFrame | None) -> _date | None:
    """Last date a series changed value (ignores the flat forward-filled tail)."""
    if gdf is None or gdf.height == 0:
        return None
    changed = gdf.sort("Dates").filter(pl.col("Percent Complete").diff().abs() > _TOL)
    return changed["Dates"].max() if changed.height else gdf["Dates"].min()


def _prepare_data(df: pl.DataFrame, todays_date: _date | None) -> pl.DataFrame:
    """
    Normalize types, convert % to fractions, and trim projections so they
    start at the projection start date, joined to the Actual Done value on
    that date. Actual and Baseline series are NOT truncated, so they keep
    running past the projection start and the PI end.
    """
    df = (
        df
        .with_columns([
            pl.col("Dates").cast(pl.Date),
            (pl.col("Percent Complete").cast(pl.Float64, strict=False) / 100).alias("Percent Complete"),
        ])
        .drop_nulls(["Dates", "Percent Complete", "group"])
        .sort("Dates")
    )

    if todays_date is None:
        return df

    is_projection = pl.col("group").is_in(PROJECTION_GROUPS)
    projection_df = df.filter(is_projection & (pl.col("Dates") > todays_date))
    other_df      = df.filter(~is_projection)

    done_to_date = other_df.filter(
        (pl.col("group") == "Actual_Completed") & (pl.col("Dates") <= todays_date)
    )

    # Only join a projection that exists (the scenario chart passes actuals only)
    if done_to_date.height > 0 and projection_df.height > 0:
        todays_pct = done_to_date["Percent Complete"][-1]
        groups     = sorted(PROJECTION_GROUPS)
        stitch_rows = pl.DataFrame(
            {
                "Dates":            [todays_date] * len(groups),
                "Percent Complete": [todays_pct]  * len(groups),
                "group":            groups,
            },
            schema={"Dates": pl.Date, "Percent Complete": pl.Float64, "group": pl.Utf8},
        ).select(df.columns)
        projection_df = pl.concat([stitch_rows, projection_df])

    return pl.concat([other_df, projection_df]).sort("Dates")


def _extend_series_to(df: pl.DataFrame, groups, until: _date | None) -> pl.DataFrame:
    """
    Hold each series flat at its last value out to `until`, so it stays
    visible alongside the projection instead of stopping where the data ends.
    """
    if until is None:
        return df
    extensions = []
    for g in groups:
        gdf = df.filter(pl.col("group") == g)
        if gdf.height and gdf["Dates"].max() < until:
            last_row = gdf.sort("Dates").tail(1)
            extensions.append(last_row.with_columns(pl.lit(until, dtype=pl.Date).alias("Dates")))
    return pl.concat([df, *extensions]).sort("Dates") if extensions else df


def _add_projection_band(fig: go.Figure, grouped: dict) -> None:
    p05_df = grouped.get("projection_05")
    p95_df = grouped.get("projection_95")
    if p05_df is None or p95_df is None:
        return

    common_df = p05_df.join(p95_df, on="Dates", how="inner", suffix="_p95").sort("Dates")
    if not common_df.height:
        return

    dates_np = common_df["Dates"].to_numpy()
    p05_vals = common_df["Percent Complete"].to_numpy()
    p95_vals = common_df["Percent Complete_p95"].to_numpy()

    fig.add_trace(go.Scatter(
        x=np.concatenate([dates_np, dates_np[::-1]]).tolist(),
        y=np.concatenate([p05_vals, p95_vals[::-1]]).tolist(),
        name="P5 – P95 range",
        mode="lines",
        line=dict(width=0),
        fill="toself",
        fillcolor=COLORS["band"],
        hoverinfo="skip",
        showlegend=True,
    ))


def _add_core_series(fig: go.Figure, grouped: dict) -> None:
    for group_name, group_df in grouped.items():
        if group_name in SKIP_GROUPS:
            continue

        display = DISPLAY_NAMES.get(group_name, group_name)
        is_proj = group_name == "projection_median"

        fig.add_trace(go.Scatter(
            x=group_df["Dates"].to_list(),
            y=group_df["Percent Complete"].to_list(),
            name=display,
            mode="lines",
            line=dict(
                color=COLORS.get(group_name),
                width=2.5,
                dash="dot" if is_proj else "solid",
            ),
            hovertemplate=(
                "<b>%{x|%b %d, %Y}</b><br>"
                + display
                + ": %{y:.1%}<extra></extra>"
            ),
        ))


def _add_vline(fig: go.Figure, x_val: _date, color: str, label: str, xanchor: str = "center") -> None:
    fig.add_shape(
        type="line", xref="x", yref="paper",
        x0=str(x_val), x1=str(x_val), y0=0, y1=1,
        line=dict(color=color, width=1.5, dash="dash"),
    )
    fig.add_annotation(
        x=str(x_val), y=1, xref="x", yref="paper",
        text=label,
        showarrow=False,
        yanchor="bottom",
        xanchor=xanchor,
        font=dict(size=11, color=color),
        bgcolor="rgba(255,255,255,0.85)",
        borderpad=3,
    )


def _add_markers(fig, series_df, dates, name, color, hover_label) -> None:
    wanted = _to_dates(dates)
    if not wanted or series_df is None:
        return
    points = series_df.filter(pl.col("Dates").is_in(wanted))
    if points.height == 0:
        return
    fig.add_trace(go.Scatter(
        x=points["Dates"].to_list(),
        y=points["Percent Complete"].to_list(),
        name=name,
        mode="markers",
        marker=dict(color=color, size=9, symbol="circle", line=dict(color="#FFFFFF", width=1.5)),
        hovertemplate="<b>%{x|%b %d, %Y}</b><br>" + hover_label + ": %{y:.1%}<extra></extra>",
        showlegend=True,
    ))


# =============================================================================
# Base figure (shared by both public builders)
# =============================================================================

def _build_base_figure(df: pl.DataFrame, todays_date=None, proj_comp_date=None, extend_until=None):
    """
    extend_until: optional date to carry the actual lines (and the x-axis) out
    to, for callers that draw their own projections (the scenario chart).
    """
    todays_date    = _to_date(todays_date)
    proj_comp_date = _to_date(proj_comp_date)

    df = _prepare_data(df, todays_date)

    # When the lower projection band (P5) reaches its peak: 100%, or the
    # highest value it ever gets to. Falls back to the median if P5 is absent.
    lower_df = df.filter(pl.col("group") == "projection_05")
    if lower_df.height == 0:
        lower_df = df.filter(pl.col("group") == "projection_median")
    lower_proj_end = _first_date_at_max(lower_df) if lower_df.height else None

    # The projection ends there: past that date every band is flat, so trim it
    if lower_proj_end is not None:
        df = df.filter(
            ~pl.col("group").is_in(PROJECTION_GROUPS) | (pl.col("Dates") <= lower_proj_end)
        )

    extend_until = _to_date(extend_until)
    if extend_until is not None:
        lower_proj_end = max(d for d in (lower_proj_end, extend_until) if d is not None)

    # Carry the actual lines forward to that date
    df = _extend_series_to(df, EXTEND_GROUPS, lower_proj_end)

    # Does the median projection ever reach 100%? If not, we mark where the
    # projection tops out with a "Projected maximum" line.
    median_rows = df.filter(pl.col("group") == "projection_median")
    median_peak = float(median_rows["Percent Complete"].max()) if median_rows.height else None
    show_plateau = (
        median_peak is not None
        and median_peak < COMPLETE_THRESHOLD
        and lower_proj_end is not None
    )

    grouped = {name: gdf for (name,), gdf in df.group_by("group", maintain_order=True)}
    fig     = go.Figure()

    _add_projection_band(fig, grouped)
    _add_core_series(fig, grouped)

    baseline_df = grouped.get("Baseline")
    median_df   = grouped.get("projection_median")
    plot_max    = float(df["Percent Complete"].max()) if df.height else 1.0

    # ── Key dates ─────────────────────────────────────────────────────────────
    # PI start: last date Baseline is still 0%.  PI end: first date it hits max.
    pi_start = None
    if baseline_df is not None:
        zero_rows = baseline_df.filter(pl.col("Percent Complete") <= _TOL)
        pi_start  = zero_rows["Dates"].max() if zero_rows.height else baseline_df["Dates"].min()
    pi_end   = _first_date_at_max(baseline_df)
    proj_end = _first_date_at_max(median_df)

    # Last day any actual series actually moved (e.g. carry-over work that
    # finishes after the PI ends)
    last_actual_change = max(
        (d for d in (_last_change_date(grouped.get(g)) for g in ACTUAL_GROUPS) if d is not None),
        default=None,
    )

    # ── X-axis range ──────────────────────────────────────────────────────────
    # Starts 1 week before PI start. Ends at the latest of: PI end, median
    # projection completion, and the last change in actuals, each padded, so
    # nothing that is still moving gets clipped at the PI end date.
    min_date = (pi_start - _td(weeks=1)) if pi_start else df["Dates"].min()

    end_candidates = [
        pi_end             + PAD_AFTER_PI_END      if pi_end             else None,
        proj_end           + PAD_AFTER_PROJ_END    if proj_end           else None,
        lower_proj_end     + PAD_AFTER_PROJ_END    if lower_proj_end     else None,
        last_actual_change + PAD_AFTER_LAST_ACTUAL if last_actual_change else None,
        todays_date        + _td(weeks=1)          if todays_date        else None,
        proj_comp_date     + _td(weeks=1)          if proj_comp_date     else None,
    ]
    end_candidates = [d for d in end_candidates if d is not None]
    max_date = max(end_candidates) if end_candidates else df["Dates"].max()

    # If the projected-maximum line is the latest event on the chart, end the
    # axis right there instead of padding past it.
    if show_plateau:
        other_events = [d for d in (pi_end, proj_end, last_actual_change,
                                    todays_date, proj_comp_date) if d is not None]
        if not other_events or lower_proj_end >= max(other_events):
            max_date = lower_proj_end + PAD_AFTER_PLATEAU

    # ── Vertical marker lines ─────────────────────────────────────────────────
    vlines = []
    if todays_date is not None and todays_date == pi_end:
        vlines.append((todays_date, COLORS["pi_end_line"], "PI end date / Projection Start"))
    else:
        if todays_date is not None:
            vlines.append((todays_date, COLORS["today_line"], "Today"))
        if pi_end is not None:
            vlines.append((pi_end, COLORS["pi_end_line"], "PI end date"))
    if proj_comp_date is not None:
        vlines.append((proj_comp_date, COLORS["comp_line"], "Target Completion"))
    if pi_start is not None:
        vlines.append((pi_start, COLORS["pi_beg_line"], "PI start"))

    for x_val, color, label in vlines:
        _add_vline(fig, x_val, color, label)

    if show_plateau:
        # Right-aligned so the label stays inside the plot at the axis edge
        _add_vline(
            fig, lower_proj_end, COLORS["plateau_line"],
            f"Projected maximum: {median_peak:.0%}",
            xanchor="right",
        )

    # ── Layout ────────────────────────────────────────────────────────────────
    fig.update_layout(
        template="plotly_white",
        paper_bgcolor=COLORS["paper"],
        plot_bgcolor=COLORS["bg"],

        title=dict(
            text="PI Progress & Projection",
            x=0.5,
            font=dict(size=18, color="#111827", family="'DM Sans', Arial, sans-serif"),
            pad=dict(b=16),
        ),
        font=dict(family="'DM Sans', Arial, sans-serif", size=13, color="#374151"),

        xaxis=dict(
            title=None,
            tickformat="%b %d",
            tickfont=dict(size=11, color="#6B7280"),
            showgrid=False,
            zeroline=False,
            showline=True,
            linecolor="rgba(156, 163, 175, 0.4)",
            linewidth=1,
            range=[str(min_date), str(max_date)],
        ),
        yaxis=dict(
            title=None,
            tickformat=".0%",
            tickfont=dict(size=11, color="#6B7280"),
            gridcolor=COLORS["grid"],
            gridwidth=1,
            zeroline=False,
            showline=False,
            # At least 0–105%; grows if actuals run past 100%
            range=[0, max(1.05, plot_max * 1.05)],
        ),

        legend=dict(
            orientation="h",
            yanchor="bottom", y=1.04,
            xanchor="center", x=0.5,
            font=dict(size=12, color="#374151"),
            bgcolor="rgba(255,255,255,0)",
            borderwidth=0,
            traceorder="normal",
        ),

        margin=dict(l=48, r=32, t=80, b=48),
        height=460,
        hovermode="x unified",
    )

    # Light horizontal rule at 100% for visual reference
    fig.add_shape(
        type="line", xref="paper", yref="y",
        x0=0, x1=1, y0=1.0, y1=1.0,
        line=dict(color="rgba(156,163,175,0.35)", width=1, dash="dot"),
    )

    return fig, grouped


# =============================================================================
# Public builders
# =============================================================================

def build_figure_pi_display(df: pl.DataFrame, todays_date=None, proj_comp_date=None,
                            wip_dates: list[str] = None, sprint_dates: list[str] = None):
    function_name = inspect.currentframe().f_code.co_name
    start_time = time.time()

    fig, _ = _build_base_figure(df, todays_date, proj_comp_date)

    print(f"{function_name} took {time.time() - start_time:.3f}s")
    return fig


def build_figure_pi_display_initial(df: pl.DataFrame, todays_date=None, proj_comp_date=None,
                                    wip_dates: list[str] = None, sprint_dates: list[str] = None):
    function_name = inspect.currentframe().f_code.co_name
    start_time = time.time()

    fig, grouped = _build_base_figure(df, todays_date, proj_comp_date)

    # WIP alert figure: markers on the In Progress line where WIP exceeded the limit
    wip_fig = go.Figure(fig)
    _add_markers(
        wip_fig, grouped.get("Actual_In_Progress"), wip_dates,
        name="WIP marker", color=COLORS["Actual_In_Progress"], hover_label="WIP excess",
    )

    # Sprint alert figure: markers on the Baseline where planned SP spiked
    sprint_fig = go.Figure(fig)
    _add_markers(
        sprint_fig, grouped.get("Baseline"), sprint_dates,
        name="Sprint Excess marker", color=COLORS["Baseline"], hover_label="Baseline",
    )

    print(f"{function_name} took {time.time() - start_time:.3f}s")
    return fig, wip_fig, sprint_fig


# =============================================================================
# Scenario comparison (Tab 3)
# =============================================================================

def _scenario_projection(df: pl.DataFrame, todays_date: _date | None):
    """A scenario's projection rows (as fractions), joined to Actual Done at the
    projection start and trimmed where its lower band peaks."""
    prepared = _prepare_data(df, todays_date)
    proj = prepared.filter(pl.col("group").is_in(PROJECTION_GROUPS))

    lower = proj.filter(pl.col("group") == "projection_05")
    if lower.height == 0:
        lower = proj.filter(pl.col("group") == "projection_median")
    end = _first_date_at_max(lower) if lower.height else None
    if end is not None:
        proj = proj.filter(pl.col("Dates") <= end)
    return proj, end


def build_figure_scenario_comparison(scenarios: list[dict], todays_date=None):
    """
    Draw several projections for the same PI on one chart.

    scenarios : list of {"label": str, "figure_df": pl.DataFrame}
                (figure_df as produced by prepare_figure_data). Actuals,
                baseline and PI markers are taken from the first scenario;
                each scenario contributes its own P5–P95 band and median.
    """
    function_name = inspect.currentframe().f_code.co_name
    start_time    = time.time()
    todays_date   = _to_date(todays_date)

    projections = [_scenario_projection(sc["figure_df"], todays_date) for sc in scenarios]
    ends        = [end for _, end in projections if end is not None]

    base_df = scenarios[0]["figure_df"].filter(~pl.col("group").is_in(PROJECTION_GROUPS))
    fig, _  = _build_base_figure(base_df, todays_date, None, extend_until=max(ends) if ends else None)

    for i, (sc, (proj, _end)) in enumerate(zip(scenarios, projections)):
        style  = SCENARIO_STYLES[i % len(SCENARIO_STYLES)]
        label  = sc["label"]
        group  = f"scenario-{i}"
        by_grp = {name: gdf.sort("Dates") for (name,), gdf in proj.group_by("group")}

        # P5–P95 band
        p05, p95 = by_grp.get("projection_05"), by_grp.get("projection_95")
        if p05 is not None and p95 is not None:
            band = p05.join(p95, on="Dates", how="inner", suffix="_p95").sort("Dates")
            if band.height:
                dates = band["Dates"].to_list()
                fig.add_trace(go.Scatter(
                    x=dates + dates[::-1],
                    y=band["Percent Complete"].to_list() + band["Percent Complete_p95"].to_list()[::-1],
                    name="P5–P95 range", legendgroup=group, legendgrouptitle_text=label,
                    mode="lines", line=dict(width=0), fill="toself", fillcolor=style["band"],
                    hoverinfo="skip",
                ))

        # Median line, labelled at its end with where it lands
        median = by_grp.get("projection_median")
        if median is None or median.height == 0:
            continue
        fig.add_trace(go.Scatter(
            x=median["Dates"].to_list(), y=median["Percent Complete"].to_list(),
            name="Median", legendgroup=group, legendgrouptitle_text=label, mode="lines",
            line=dict(color=style["color"], width=2.5, dash=style["dash"]),
            hovertemplate="<b>%{x|%b %d, %Y}</b><br>" + label + ": %{y:.1%}<extra></extra>",
        ))

        peak      = float(median["Percent Complete"].max())
        finish    = _first_date_at_max(median)
        last_date = median["Dates"][-1]
        last_pct  = float(median["Percent Complete"][-1])
        outcome   = (f"done {finish:%b} {finish.day}" if peak >= COMPLETE_THRESHOLD
                     else f"levels off at {peak:.0%}")
        fig.add_annotation(
            x=str(last_date), y=last_pct, xref="x", yref="y",
            text=f"<b>{label}</b><br>{outcome}", showarrow=False,
            xanchor="left", xshift=6,
            yanchor="bottom" if i == 0 else "top", yshift=4 if i == 0 else -4,
            align="left", font=dict(size=11, color=style["color"]),
            bgcolor="rgba(255,255,255,0.85)", borderpad=2,
        )

    # Actuals and plan share one legend heading
    fig.update_traces(
        selector=lambda t: t.name in {"Actual Done", "Actual In Progress", "Baseline"},
        legendgroup="actuals", legendgrouptitle_text="Actuals and plan",
    )

    # Room on the right for the end-of-line labels
    x_range = list(fig.layout.xaxis.range)
    x_range[1] = str(_to_date(x_range[1]) + _td(weeks=2))

    fig.update_layout(
        title=dict(text="Scenario comparison", x=0, xanchor="left", pad=dict(l=8, b=8)),
        xaxis=dict(range=x_range),
        height=500,
        # Legend in its own panel to the right of the plot
        legend=dict(
            orientation="v",
            x=1.02, xanchor="left",
            y=1.0, yanchor="top",
            traceorder="grouped",
            groupclick="toggleitem",   # items toggle alone; click a heading to toggle its group
            tracegroupgap=14,
            bgcolor="#FFFFFF",
            bordercolor="#E5E7EB",
            borderwidth=1,
            font=dict(size=12, color="#374151"),
            grouptitlefont=dict(size=12, color="#1F2937"),
            itemwidth=40,
        ),
        margin=dict(l=48, r=210, t=64, b=48),
    )

    print(f"{function_name} took {time.time() - start_time:.3f}s")
    return fig
