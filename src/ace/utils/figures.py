import plotly.graph_objects as go
import polars as pl
import numpy as np
from datetime import date as _date, datetime as _datetime, timedelta as _td
import time
import inspect


def build_figure_pi_display(df: pl.DataFrame, todays_date=None, proj_comp_date=None, wip_dates: list[str] = None, sprint_dates: list[str] = None):
    function_name = inspect.currentframe().f_code.co_name
    start_time = time.time()

    PROJECTION_GROUPS = frozenset({"projection_median", "projection_05", "projection_95"})
    SKIP_GROUPS       = frozenset({"projection_05", "projection_95", "SprintStep"})
    CORE_KEEP         = frozenset({"Baseline", "Actual", "projection_median"})

    # ── Refined palette ───────────────────────────────────────────────────────
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

    # ── 1) Clean and prepare data ─────────────────────────────────────────────
    df = (
        df
        .with_columns([
            pl.col("Dates").cast(pl.Date),
            (pl.col("Percent Complete").cast(pl.Float64, strict=False) / 100).alias("Percent Complete"),
        ])
        .drop_nulls(["Dates", "Percent Complete", "group"])
        .sort("Dates")
    )

    print(df)

    # ── 1b) Truncate based on today's date ────────────────────────────────────
    if todays_date is not None:
        if isinstance(todays_date, str):
            todays_date = _datetime.strptime(todays_date, "%Y-%m-%d").date()
        elif isinstance(todays_date, _datetime):
            todays_date = todays_date.date()

        is_actual     = pl.col("group") == "Actual"
        is_projection = pl.col("group").is_in(PROJECTION_GROUPS)

        actual_df     = df.filter(is_actual     & (pl.col("Dates") <= todays_date))
        projection_df = df.filter(is_projection & (pl.col("Dates") >=  todays_date))
        other_df      = df.filter(~is_actual    & ~is_projection)

        if actual_df.height > 0:
            last_actual   = actual_df.tail(1)
            todays_pct    = last_actual["Percent Complete"][0]

            stitch_rows = pl.DataFrame({
                "Dates":            [todays_date] * len(PROJECTION_GROUPS),  
                "Percent Complete": [todays_pct]  * len(PROJECTION_GROUPS),
                "group":            list(PROJECTION_GROUPS),
            }, schema={"Dates": pl.Date, "Percent Complete": pl.Float64, "group": pl.Utf8})

            stitch_rows   = stitch_rows.select(projection_df.columns)
            projection_df = pl.concat([stitch_rows, projection_df]).sort("Dates")

        df = pl.concat([actual_df, projection_df, other_df]).sort("Dates")

    # ── Polars-native grouping (replaces pandas to_pandas()/groupby) ───────────
    grouped = {name: gdf for (name,), gdf in df.group_by("group", maintain_order=True)}
    fig     = go.Figure()

    # ── 2) Shaded projection band ─────────────────────────────────────────────
    p05_df = grouped.get("projection_05")
    p95_df = grouped.get("projection_95")

    if p05_df is not None and p95_df is not None:
        common_df = (
            p05_df.join(p95_df, on="Dates", how="inner", suffix="_p95")
            .sort("Dates")
        )

        if common_df.height:
            dates_np = common_df["Dates"].to_numpy()
            p05_vals = common_df["Percent Complete"].to_numpy()
            p95_vals = common_df["Percent Complete_p95"].to_numpy()

            x_poly = np.concatenate([dates_np, dates_np[::-1]]).tolist()
            y_poly = np.concatenate([p05_vals, p95_vals[::-1]]).tolist()

            fig.add_trace(go.Scatter(
                x=x_poly, y=y_poly,
                name="P5 – P95 range",
                mode="lines",
                line=dict(width=0),
                fill="toself",
                fillcolor=COLORS["band"],
                hoverinfo="skip",
                showlegend=True,
            ))

    # ── 3) Core series ────────────────────────────────────────────────────────
    for group_name, group_df in grouped.items():
        if group_name in SKIP_GROUPS:
            continue
        if group_name not in CORE_KEEP and (
            group_name.startswith("projection_") or group_name == "SprintStep"
        ):
            continue

        color   = COLORS.get(group_name)
        display = DISPLAY_NAMES.get(group_name, group_name)
        is_proj = group_name == "projection_median"

        fig.add_trace(go.Scatter(
            x=group_df["Dates"].to_list(),
            y=group_df["Percent Complete"].to_list(),
            name=display,
            mode="lines",
            line=dict(
                color=color,
                width=2.5,
                dash="dot" if is_proj else "solid",
            ),
            hovertemplate=(
                "<b>%{x|%b %d, %Y}</b><br>"
                + display
                + ": %{y:.1%}<extra></extra>"
            ),
        ))

    # ── 3b) Vertical marker lines ─────────────────────────────────────────────
    actual_pct        =  grouped["Actual"]["Percent Complete"]   if "Actual"   in grouped else None
    baseline_pct      =  grouped["Baseline"]["Percent Complete"] if "Baseline" in grouped else None
    projection_50_pct =  grouped["projection_median"]["Percent Complete"] if "projection_median" in grouped else None

    actual_max     = float(actual_pct.max())   if actual_pct   is not None else 0.0
    baseline_max   = float(baseline_pct.max()) if baseline_pct is not None else 0.0
    projection_max = float(projection_50_pct.max()) if projection_50_pct is not None else 0.0
    plot_max       = float(df.select(pl.col("Percent Complete").max()).item())

    td_str = str(todays_date) if todays_date is not None else None 
    comp_str = str(proj_comp_date) if proj_comp_date is not None else None

    # Last date where Baseline is still 0% → PI beginning
    pi_beg_str = None
    if baseline_pct is not None:
        baseline_df = grouped["Baseline"]
        pi_beg_val = (
            baseline_df.filter(pl.col("Percent Complete") == 0.0)
            .select(pl.col("Dates").max())
            .item()
        )
        pi_beg_str = str(pi_beg_val) if pi_beg_val is not None else None

    # First date where Baseline hits its maximum value → PI end
    pi_end_str = None
    if baseline_pct is not None:
        baseline_df = grouped["Baseline"]
        pi_end_val = (
            baseline_df.filter(pl.col("Percent Complete") == baseline_max)
            .select(pl.col("Dates").min())
            .item()
        )
        pi_end_str = str(pi_end_val) if pi_end_val is not None else None

    projection_end_str = None
    if projection_50_pct is not None: 
        projection_50_df = grouped["projection_median"]
        projection_end_val = (
            projection_50_df.filter(pl.col("Percent Complete") == projection_max)
            .select(pl.col("Dates").min())
            .item()
        )
        projection_end_str = str(projection_end_val) if projection_end_val is not None else None

    # ── Compute x-axis bounds ─────────────────────────────────────────────────

    # Start 1 week before PI beginning
    if pi_beg_str is not None:
        min_date = _date.fromisoformat(pi_beg_str[:10]) - _td(weeks=1)
    else:
        min_date = df.select(pl.col("Dates").min()).item()

    # End = later of (PI end + 2w) and (median projection end + 2w)
    # Each term is computed independently so a missing value doesn't crash.
    pi_end_plus2   = (
        _date.fromisoformat(pi_end_str[:10]) + _td(weeks=2)
        if pi_end_str is not None
        else None
    )
    proj_end_plus2 = (
        _date.fromisoformat(projection_end_str[:10]) + _td(weeks=2)
        if projection_end_str is not None
        else None
    )

    candidates = [d for d in [pi_end_plus2, proj_end_plus2] if d is not None]
    max_date   = max(candidates) if candidates else df.select(pl.col("Dates").max()).item()

    x_range = [str(min_date), str(max_date)]
    
    # ── Build vertical line list ──────────────────────────────────────────────
    # Normalize to YYYY-MM-DD strings before comparing so time components
    # or None values don't cause false mismatches.
    _td_date   = td_str[:10]      if td_str      else None
    _pi_end    = pi_end_str[:10]  if pi_end_str  else None
    _comp      = comp_str[:10]    if comp_str    else None

    proj_start_is_pi_end = _td_date is not None and _td_date == _pi_end

    vlines = []

    if proj_start_is_pi_end:
        # Projection start and PI end are the same day — merge into one line
        vlines.append((td_str, "#7C3AED", "PI end date / Projection Start"))
    else:
        # Two separate lines
        if _td_date is not None:
            vlines.append((td_str, COLORS["today_line"], "Today"))
        if _pi_end is not None:
            vlines.append((pi_end_str, "#7C3AED", "PI end date"))

    # Target Completion always present, independent of the above
    if _comp is not None:
        vlines.append((comp_str, COLORS["comp_line"], "Target Completion"))

    # PI start always appended if derivable from Baseline
    if pi_beg_str is not None:
        vlines.append((pi_beg_str, COLORS["pi_beg_line"], "PI start"))


    for x_val, color, label in vlines:
        fig.add_shape(
            type="line", xref="x", yref="paper",
            x0=x_val, x1=x_val, y0=0, y1=1,
            line=dict(color=color, width=1.5, dash="dash"),
        )
        fig.add_annotation(
            x=x_val, y=1, xref="x", yref="paper",
            text=label,
            showarrow=False,
            yanchor="bottom",
            font=dict(size=11, color=color),
            bgcolor="rgba(255,255,255,0.85)",
            borderpad=3,
        )

    # ── 4) Layout ─────────────────────────────────────────────────────────────
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
            range=x_range,
        ),
        yaxis=dict(
            title=None,
            tickformat=".0%",
            tickfont=dict(size=11, color="#6B7280"),
            gridcolor=COLORS["grid"],
            gridwidth=1,
            zeroline=False,
            showline=False,
            range=[0, min(plot_max * 1.08, 1.05)],
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

    time_to_complete = time.time() - start_time
    print(f"{function_name} took {time_to_complete:.3f}s")
    return fig

def build_figure_pi_display_initial(df: pl.DataFrame, todays_date=None, proj_comp_date=None, wip_dates: list[str] = None, sprint_dates: list[str] = None):
    function_name = inspect.currentframe().f_code.co_name
    start_time = time.time()

    PROJECTION_GROUPS = frozenset({"projection_median", "projection_05", "projection_95"})
    SKIP_GROUPS       = frozenset({"projection_05", "projection_95", "SprintStep"})
    CORE_KEEP         = frozenset({"Baseline", "Actual", "projection_median"})

    # ── Refined palette ───────────────────────────────────────────────────────
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

    # ── 1) Clean and prepare data ─────────────────────────────────────────────
    df = (
        df
        .with_columns([
            pl.col("Dates").cast(pl.Date),
            (pl.col("Percent Complete").cast(pl.Float64, strict=False) / 100).alias("Percent Complete"),
        ])
        .drop_nulls(["Dates", "Percent Complete", "group"])
        .sort("Dates")
    )

    print(df)

    # ── 1b) Truncate based on today's date ────────────────────────────────────
    if todays_date is not None:
        if isinstance(todays_date, str):
            todays_date = _datetime.strptime(todays_date, "%Y-%m-%d").date()
        elif isinstance(todays_date, _datetime):
            todays_date = todays_date.date()

        is_actual     = pl.col("group") == "Actual"
        is_projection = pl.col("group").is_in(PROJECTION_GROUPS)

        actual_df     = df.filter(is_actual     & (pl.col("Dates") <= todays_date))
        projection_df = df.filter(is_projection & (pl.col("Dates") >=  todays_date))
        other_df      = df.filter(~is_actual    & ~is_projection)

        if actual_df.height > 0:
            last_actual   = actual_df.tail(1)
            todays_pct    = last_actual["Percent Complete"][0]

            stitch_rows = pl.DataFrame({
                "Dates":            [todays_date] * len(PROJECTION_GROUPS),  
                "Percent Complete": [todays_pct]  * len(PROJECTION_GROUPS),
                "group":            list(PROJECTION_GROUPS),
            }, schema={"Dates": pl.Date, "Percent Complete": pl.Float64, "group": pl.Utf8})

            stitch_rows   = stitch_rows.select(projection_df.columns)
            projection_df = pl.concat([stitch_rows, projection_df]).sort("Dates")

        df = pl.concat([actual_df, projection_df, other_df]).sort("Dates")

    # ── Polars-native grouping (replaces pandas to_pandas()/groupby) ───────────
    grouped = {name: gdf for (name,), gdf in df.group_by("group", maintain_order=True)}
    fig     = go.Figure()

    # ── 2) Shaded projection band ─────────────────────────────────────────────
    p05_df = grouped.get("projection_05")
    p95_df = grouped.get("projection_95")

    if p05_df is not None and p95_df is not None:
        common_df = (
            p05_df.join(p95_df, on="Dates", how="inner", suffix="_p95")
            .sort("Dates")
        )

        if common_df.height:
            dates_np = common_df["Dates"].to_numpy()
            p05_vals = common_df["Percent Complete"].to_numpy()
            p95_vals = common_df["Percent Complete_p95"].to_numpy()

            x_poly = np.concatenate([dates_np, dates_np[::-1]]).tolist()
            y_poly = np.concatenate([p05_vals, p95_vals[::-1]]).tolist()

            fig.add_trace(go.Scatter(
                x=x_poly, y=y_poly,
                name="P5 – P95 range",
                mode="lines",
                line=dict(width=0),
                fill="toself",
                fillcolor=COLORS["band"],
                hoverinfo="skip",
                showlegend=True,
            ))

    # ── 3) Core series ────────────────────────────────────────────────────────
    for group_name, group_df in grouped.items():
        if group_name in SKIP_GROUPS:
            continue
        if group_name not in CORE_KEEP and (
            group_name.startswith("projection_") or group_name == "SprintStep"
        ):
            continue

        color   = COLORS.get(group_name)
        display = DISPLAY_NAMES.get(group_name, group_name)
        is_proj = group_name == "projection_median"

        fig.add_trace(go.Scatter(
            x=group_df["Dates"].to_list(),
            y=group_df["Percent Complete"].to_list(),
            name=display,
            mode="lines",
            line=dict(
                color=color,
                width=2.5,
                dash="dot" if is_proj else "solid",
            ),
            hovertemplate=(
                "<b>%{x|%b %d, %Y}</b><br>"
                + display
                + ": %{y:.1%}<extra></extra>"
            ),
        ))

    # ── 3b) Vertical marker lines ─────────────────────────────────────────────
    actual_pct        =  grouped["Actual"]["Percent Complete"]   if "Actual"   in grouped else None
    baseline_pct      =  grouped["Baseline"]["Percent Complete"] if "Baseline" in grouped else None
    projection_50_pct =  grouped["projection_median"]["Percent Complete"] if "projection_median" in grouped else None

    actual_max     = float(actual_pct.max())   if actual_pct   is not None else 0.0
    baseline_max   = float(baseline_pct.max()) if baseline_pct is not None else 0.0
    projection_max = float(projection_50_pct.max()) if projection_50_pct is not None else 0.0
    plot_max       = float(df.select(pl.col("Percent Complete").max()).item())

    td_str = str(todays_date) if todays_date is not None else None 
    comp_str = str(proj_comp_date) if proj_comp_date is not None else None

    # Last date where Baseline is still 0% → PI beginning
    pi_beg_str = None
    if baseline_pct is not None:
        baseline_df = grouped["Baseline"]
        pi_beg_val = (
            baseline_df.filter(pl.col("Percent Complete") == 0.0)
            .select(pl.col("Dates").max())
            .item()
        )
        pi_beg_str = str(pi_beg_val) if pi_beg_val is not None else None

    # First date where Baseline hits its maximum value → PI end
    pi_end_str = None
    if baseline_pct is not None:
        baseline_df = grouped["Baseline"]
        pi_end_val = (
            baseline_df.filter(pl.col("Percent Complete") == baseline_max)
            .select(pl.col("Dates").min())
            .item()
        )
        pi_end_str = str(pi_end_val) if pi_end_val is not None else None

    projection_end_str = None
    if projection_50_pct is not None: 
        projection_50_df = grouped["projection_median"]
        projection_end_val = (
            projection_50_df.filter(pl.col("Percent Complete") == projection_max)
            .select(pl.col("Dates").min())
            .item()
        )
        projection_end_str = str(projection_end_val) if projection_end_val is not None else None

        
    # ── Compute x-axis bounds ─────────────────────────────────────────────────

    # Start 1 week before PI beginning
    if pi_beg_str is not None:
        min_date = _date.fromisoformat(pi_beg_str[:10]) - _td(weeks=1)
    else:
        min_date = df.select(pl.col("Dates").min()).item()

    # End = later of (PI end + 2w) and (median projection end + 2w)
    # Each term is computed independently so a missing value doesn't crash.
    pi_end_plus2   = (
        _date.fromisoformat(pi_end_str[:10]) + _td(weeks=2)
        if pi_end_str is not None
        else None
    )
    proj_end_plus2 = (
        _date.fromisoformat(projection_end_str[:10]) + _td(weeks=2)
        if projection_end_str is not None
        else None
    )

    candidates = [d for d in [pi_end_plus2, proj_end_plus2] if d is not None]
    max_date   = max(candidates) if candidates else df.select(pl.col("Dates").max()).item()

    x_range = [str(min_date), str(max_date)]
    
    # ── Build vertical line list ──────────────────────────────────────────────
    # Normalize to YYYY-MM-DD strings before comparing so time components
    # or None values don't cause false mismatches.
    _td_date   = td_str[:10]      if td_str      else None
    _pi_end    = pi_end_str[:10]  if pi_end_str  else None
    _comp      = comp_str[:10]    if comp_str    else None

    proj_start_is_pi_end = _td_date is not None and _td_date == _pi_end

    vlines = []

    if proj_start_is_pi_end:
        # Projection start and PI end are the same day — merge into one line
        vlines.append((td_str, "#7C3AED", "PI end date / Projection Start"))
    else:
        # Two separate lines
        if _td_date is not None:
            vlines.append((td_str, COLORS["today_line"], "Today"))
        if _pi_end is not None:
            vlines.append((pi_end_str, "#7C3AED", "PI end date"))

    # Target Completion always present, independent of the above
    if _comp is not None:
        vlines.append((comp_str, COLORS["comp_line"], "Target Completion"))

    # PI start always appended if derivable from Baseline
    if pi_beg_str is not None:
        vlines.append((pi_beg_str, COLORS["pi_beg_line"], "PI start"))


    for x_val, color, label in vlines:
        fig.add_shape(
            type="line", xref="x", yref="paper",
            x0=x_val, x1=x_val, y0=0, y1=1,
            line=dict(color=color, width=1.5, dash="dash"),
        )
        fig.add_annotation(
            x=x_val, y=1, xref="x", yref="paper",
            text=label,
            showarrow=False,
            yanchor="bottom",
            font=dict(size=11, color=color),
            bgcolor="rgba(255,255,255,0.85)",
            borderpad=3,
        )

    # ── 4) Layout ─────────────────────────────────────────────────────────────
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
            range=x_range,
        ),
        yaxis=dict(
            title=None,
            tickformat=".0%",
            tickfont=dict(size=11, color="#6B7280"),
            gridcolor=COLORS["grid"],
            gridwidth=1,
            zeroline=False,
            showline=False,
            range=[0, min(plot_max * 1.08, 1.05)],
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

    # ── Last) Build wip_fig and spring_fig as a copy of fig, with WIP and Sprint markers added ───────────

    wip_fig = go.Figure(fig)  # deep-ish copy: new Figure wrapping the same spec, safe to add traces to independently

    if wip_dates and "Actual_In_Progress" in grouped:
        wip_df = grouped["Actual_In_Progress"]

        # Normalize wip_dates to date objects for comparison
        parsed_wip_dates = []
        for wd in wip_dates:
            if isinstance(wd, str):
                parsed_wip_dates.append(_datetime.strptime(wd, "%Y-%m-%d").date())
            elif isinstance(wd, _date):
                parsed_wip_dates.append(wd)
            else:
                parsed_wip_dates.append(wd)

        # Dates column is already pl.Date -- no pandas Timestamp normalization needed
        marker_points = wip_df.filter(pl.col("Dates").is_in(parsed_wip_dates))

        if marker_points.height > 0:
            wip_fig.add_trace(go.Scatter(
                x=marker_points["Dates"].to_list(),
                y=marker_points["Percent Complete"].to_list(),
                name="WIP marker",
                mode="markers",
                marker=dict(
                    color=COLORS["Actual_In_Progress"],
                    size=9,
                    symbol="circle",
                    line=dict(color="#FFFFFF", width=1.5),
                ),
                hovertemplate=(
                    "<b>%{x|%b %d, %Y}</b><br>"
                    "WIP Excess: %{y:.1%}<extra></extra>"
                ),
                showlegend=True,
            ))

    sprint_fig = go.Figure(fig)  

    if sprint_dates and "Baseline" in grouped:
        sprint_df = grouped["Baseline"]

        # Normalize sprint_dates to date objects for comparison
        parsed_sprint_dates = []
        for wd in sprint_dates:
            if isinstance(wd, str):
                parsed_sprint_dates.append(_datetime.strptime(wd, "%Y-%m-%d").date())
            elif isinstance(wd, _date):
                parsed_sprint_dates.append(wd)
            else:
                parsed_sprint_dates.append(wd)

        # Dates column is already pl.Date -- no pandas Timestamp normalization needed
        marker_points = sprint_df.filter(pl.col("Dates").is_in(parsed_sprint_dates))

        if marker_points.height > 0:
            sprint_fig.add_trace(go.Scatter(
                x=marker_points["Dates"].to_list(),
                y=marker_points["Percent Complete"].to_list(),
                name="Sprint Excess marker",
                mode="markers",
                marker=dict(
                    color=COLORS["Baseline"],
                    size=9,
                    symbol="circle",
                    line=dict(color="#FFFFFF", width=1.5),
                ),
                hovertemplate=(
                    "<b>%{x|%b %d, %Y}</b><br>"
                ),
                showlegend=True,
            ))

    time_to_complete = time.time() - start_time
    print(f"{function_name} took {time_to_complete:.3f}s")
    return fig, wip_fig, sprint_fig
