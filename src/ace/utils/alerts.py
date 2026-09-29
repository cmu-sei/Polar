# ---------------------------------------------------------------------------
# alerts.py
#
# Data, figures and alert cards for the Realtime Alerts tab.
#
#   Work in progress — how much work has been started but not finished,
#                      against a limit (share of total scope).
#   Sprint load      — planned SP per sprint against what the team actually
#                      delivers per sprint, flagging sprints whose plan jumps
#                      sharply or runs well above that pace.
#
# build_alerts() returns a JSON-serialisable dict for a dcc.Store;
# render_alert_card() and rule_status() turn it into components.
# ---------------------------------------------------------------------------

from datetime import date, datetime, timedelta

import plotly.graph_objects as go
import polars as pl
from dash import html
from plotly.subplots import make_subplots

# ── Rule thresholds ───────────────────────────────────────────────────────────
WIP_LIMIT_PCT   = 20.0   # started-not-finished work, % of scope (matches Actuals.WIP_LIMIT)
SPIKE_RATIO     = 0.5    # plan more than 50% above the previous sprint (matches baseline.py)
OVER_PACE_RATIO = 1.25   # plan more than 25% above the team's delivered pace
BACKLOAD_SHARE  = 0.5    # more than half the plan in the final two sprints

# ── Palette (matches figures.py) ──────────────────────────────────────────────
_DONE    = "#1D3557"
_IN_PROG = "#4F7CAC"
_PLAN    = "#D16D6A"
_OVER    = "#9F1239"
_ALERT   = "#B91C1C"
_OK      = "#15803D"
_TODAY   = "#94A3B8"
_PI_END  = "#7C3AED"   # matches "PI end date" on the main chart
_PI_BEG  = "#1A3B55"   # matches "PI start" on the main chart
_CARRY   = "#B45309"
_OVERLAP = "rgba(180, 83, 9, 0.10)"
_DAY_MS  = 24 * 60 * 60 * 1000
_TEXT    = "#1F2937"
_MUTED   = "#6B7280"
_RULE    = "#E5E7EB"
_FONT    = "'DM Sans', Arial, sans-serif"


# =============================================================================
# Helpers
# =============================================================================

def _to_date(v) -> date | None:
    if v is None or v == "":
        return None
    if isinstance(v, datetime):
        return v.date()
    if isinstance(v, date):
        return v
    return date.fromisoformat(str(v)[:10])


def _fmt_day(d: date) -> str:
    return f"{d:%b} {d.day}"


def _daily(combined_df: pl.DataFrame) -> pl.DataFrame:
    return combined_df.with_columns(pl.col("date").cast(pl.Date)).sort("date")


def _value_on(df: pl.DataFrame, col: str, d: date) -> float:
    rows = df.filter(pl.col("date") <= d)
    return float(rows[col][-1]) if rows.height else 0.0


def _base_layout(fig: go.Figure, height: int) -> None:
    fig.update_layout(
        template="plotly_white",
        paper_bgcolor="rgba(0,0,0,0)",
        plot_bgcolor="#FAFAF9",
        font=dict(family=_FONT, size=12, color="#374151"),
        margin=dict(l=64, r=24, t=48, b=72),
        height=height,
        hovermode="x unified",
        legend=dict(orientation="h", yanchor="top", y=-0.12, xanchor="left", x=0,
                    bgcolor="rgba(0,0,0,0)"),
    )
    fig.update_xaxes(showgrid=False, linecolor="rgba(156,163,175,0.4)", tickfont=dict(color=_MUTED))
    fig.update_yaxes(gridcolor="rgba(148,163,184,0.16)", zeroline=False, tickfont=dict(color=_MUTED))


def _vline(fig: go.Figure, d: date, color: str, text: str, xanchor: str = "center", dash: str = "dash") -> None:
    fig.add_shape(type="line", xref="x", yref="paper", x0=str(d), x1=str(d), y0=0, y1=1,
                  line=dict(color=color, width=1.5, dash=dash))
    fig.add_annotation(x=str(d), y=1, xref="x", yref="paper", text=text, showarrow=False,
                       yanchor="bottom", xanchor=xanchor, font=dict(size=11, color=color),
                       bgcolor="rgba(255,255,255,0.85)", borderpad=2)


def _today_line(fig: go.Figure, d: date) -> None:
    _vline(fig, d, _TODAY, "Today")


def _increment_dates(inc: dict | None) -> tuple[date | None, date | None]:
    """(prior increment end, current increment start)."""
    if not inc:
        return None, None
    prior = inc.get("prior")
    return (_to_date(prior["end"]) if prior else None,
            _to_date((inc.get("current") or {}).get("start")))


def _increment_markers(fig: go.Figure, inc: dict | None, x_start: date | None = None) -> None:
    """Mark where the prior increment ends and the current one begins.
    The prior increment's end is only drawn if it falls inside the chart."""
    prior_end, cur_start = _increment_dates(inc)
    if cur_start is None:
        return
    if prior_end is not None and x_start is not None and prior_end < x_start:
        prior_end = None
    cur_label   = inc["current"]["label"]
    prior_label = inc["prior"]["label"] if inc.get("prior") and prior_end is not None else None

    if prior_end is not None and prior_end > cur_start:
        # Overlap: shade the days both increments are running
        fig.add_shape(type="rect", xref="x", yref="paper", x0=str(cur_start), x1=str(prior_end),
                      y0=0, y1=1, fillcolor=_OVERLAP, line_width=0, layer="below")

    if prior_end is not None and prior_end == cur_start:
        _vline(fig, cur_start, _PI_BEG, f"{prior_label} ends, {cur_label} starts", dash="solid")
        return

    # The chart starts at the current PI, so its label reads to the right.
    # When the prior PI runs past that start, its end line is drawn unlabelled
    # (a few days in, a second label would collide) and named in this one.
    overlap = prior_end is not None and prior_end > cur_start
    cur_text = f"{cur_label} starts"
    if overlap:
        fig.add_shape(type="line", xref="x", yref="paper", x0=str(prior_end), x1=str(prior_end),
                      y0=0, y1=1, line=dict(color=_PI_END, width=1.5))
        cur_text += f" ({prior_label} runs to {_fmt_day(prior_end)})"
    elif prior_end is not None:
        _vline(fig, prior_end, _PI_END, f"{prior_label} ends", xanchor="right", dash="solid")
    _vline(fig, cur_start, _PI_BEG, cur_text, xanchor="left", dash="solid")


def _window_start(inc: dict | None, fallback: date) -> date:
    """Charts on this tab start on the first day of the current PI."""
    _, cur_start = _increment_dates(inc)
    return cur_start or fallback


# =============================================================================
# Work in progress
# =============================================================================

def _build_wip(combined_df: pl.DataFrame, as_of: date, inc: dict | None = None) -> dict:
    df = _daily(combined_df).filter(pl.col("date") <= as_of)

    # Start the view shortly before work began
    active = df.filter(pl.col("in_progress_sp") > 0)
    first  = (active["date"][0] if active.height else (df["date"][0] if df.height else as_of)) - timedelta(days=7)
    x_start = _window_start(inc, first)
    df = df.filter(pl.col("date") >= x_start)

    df = df.with_columns([
        (pl.col("in_progress_sp")  - pl.col("completed_sp")).alias("gap_sp"),
        (pl.col("in_progress_pct") - pl.col("completed_pct")).alias("gap_pct"),
    ])

    # Total scope, recovered from any row where both SP and % are known
    scoped = df.filter(pl.col("in_progress_pct") > 0)
    total_sp = float((scoped["in_progress_sp"] / scoped["in_progress_pct"] * 100)[-1]) if scoped.height else 0.0
    limit_sp = WIP_LIMIT_PCT / 100 * total_sp

    dates  = df["date"].to_list()
    done   = df["completed_sp"].to_list()
    prog   = df["in_progress_sp"].to_list()
    gap    = df["gap_sp"].to_list()
    over   = [g > limit_sp for g in gap]
    excess = [g if o else limit_sp for g, o in zip(gap, over)]

    # ── Figure: two panels sharing the date axis ─────────────────────────────
    fig = make_subplots(rows=2, cols=1, shared_xaxes=True, vertical_spacing=0.1, row_heights=[0.55, 0.45])
    fig.add_trace(go.Scatter(x=dates, y=done, name="Done", mode="lines",
                             line=dict(color=_DONE, width=2.5),
                             hovertemplate="%{y:.0f} SP<extra>Done</extra>"), row=1, col=1)
    fig.add_trace(go.Scatter(x=dates, y=prog, name="Started", mode="lines",
                             line=dict(color=_IN_PROG, width=2.5),
                             fill="tonexty", fillcolor="rgba(79,124,172,0.18)",
                             hovertemplate="%{y:.0f} SP<extra>Started</extra>"), row=1, col=1)

    fig.add_trace(go.Scatter(x=dates, y=gap, name="Unfinished", mode="lines",
                             line=dict(color=_IN_PROG, width=2), fill="tozeroy",
                             fillcolor="rgba(79,124,172,0.18)", showlegend=False,
                             hovertemplate="%{y:.0f} SP<extra>Unfinished</extra>"), row=2, col=1)
    fig.add_trace(go.Scatter(x=dates, y=[limit_sp] * len(dates), name=f"Limit ({WIP_LIMIT_PCT:.0f}% of scope)",
                             mode="lines", line=dict(color=_ALERT, width=1.5, dash="dash"),
                             hovertemplate="%{y:.0f} SP<extra>Limit</extra>"), row=2, col=1)
    fig.add_trace(go.Scatter(x=dates, y=excess, name="Over limit", mode="lines",
                             line=dict(width=0), fill="tonexty", fillcolor="rgba(185,28,28,0.28)",
                             hoverinfo="skip"), row=2, col=1)

    _base_layout(fig, height=540)
    fig.update_yaxes(rangemode="tozero")
    fig.update_yaxes(title=dict(text="Started vs done (SP)", font=dict(size=12, color=_MUTED)), row=1, col=1)
    fig.update_yaxes(title=dict(text="Unfinished (SP)",      font=dict(size=12, color=_MUTED)), row=2, col=1)
    fig.update_xaxes(range=[str(x_start), str(as_of + timedelta(days=3))])
    _increment_markers(fig, inc, x_start)
    _today_line(fig, as_of)

    # ── Alert text ────────────────────────────────────────────────────────────
    if not dates:
        return {"figure": fig.to_plotly_json(), "alert": {
            "triggered": False, "headline": "No work started yet",
            "lines": ["The work-in-progress check starts once the first issue enters In Progress."],
            "action": None, "status": "No data"}}

    cur_gap, cur_pct = gap[-1], float(df["gap_pct"][-1])
    days_over = sum(over)
    triggered = over[-1]

    # Start of the current run over the limit
    since = None
    if triggered:
        i = len(over) - 1
        while i > 0 and over[i - 1]:
            i -= 1
        since = dates[i]

    peak_i = max(range(len(gap)), key=lambda i: gap[i])

    lines = [
        f"On {_fmt_day(as_of)}, {cur_gap:.0f} SP had been started but not finished: "
        f"{cur_pct:.0f}% of scope against a {WIP_LIMIT_PCT:.0f}% limit ({limit_sp:.0f} SP).",
    ]
    if triggered:
        lines.append(f"It has been over the limit every day since {_fmt_day(since)}, "
                     f"and on {days_over} of {len(dates)} days overall.")
    elif days_over:
        lines.append(f"It was over the limit on {days_over} of {len(dates)} days, "
                     f"peaking at {gap[peak_i]:.0f} SP on {_fmt_day(dates[peak_i])}.")
    else:
        lines.append(f"It has stayed under the limit, peaking at {gap[peak_i]:.0f} SP "
                     f"on {_fmt_day(dates[peak_i])}.")

    return {"figure": fig.to_plotly_json(), "alert": {
        "triggered": triggered,
        "headline":  "Too much work in progress" if triggered else "Work in progress is within the limit",
        "lines":     lines,
        "action":    ("Unfinished work spreads the team thin and delays completion. "
                      "Finish started work before pulling in new issues.") if triggered else None,
        "status":    "Over limit" if triggered else "Within limit",
    }}


# =============================================================================
# Sprint load
# =============================================================================

def _build_sprint_load(sprint_plan: dict | None, combined_df: pl.DataFrame, as_of: date,
                       inc: dict | None = None) -> dict:
    sprints = (sprint_plan or {}).get("sprints") or []
    if not sprints:
        fig = go.Figure()
        _base_layout(fig, height=420)
        return {"figure": fig.to_plotly_json(), "alert": {
            "triggered": False, "headline": "No sprint plan available",
            "lines": ["Sprint load needs sprints named with the selected PI."],
            "action": None, "status": "No data"}}

    df = _daily(combined_df)

    rows = []
    prev_sp = None
    for s in sprints:
        start, end = _to_date(s["start"]), _to_date(s["end"])
        started    = start <= as_of
        delivered  = (_value_on(df, "completed_sp", min(end, as_of)) - _value_on(df, "completed_sp", start)) if started else None
        rows.append({
            "label":     s["label"],
            "start":     start,
            "end":       end,
            "planned":   s["sp"],
            "delivered": delivered,
            "complete":  end <= as_of,
            "vs_prev":   (s["sp"] / prev_sp) if prev_sp else None,
        })
        prev_sp = s["sp"]

    # Sustainable pace: average SP delivered per finished sprint
    finished = [r["delivered"] for r in rows if r["complete"] and r["delivered"] is not None]
    pace = sum(finished) / len(finished) if finished else None

    for r in rows:
        r["spike"]     = r["vs_prev"] is not None and r["vs_prev"] > 1 + SPIKE_RATIO
        r["over_pace"] = pace is not None and pace > 0 and r["planned"] > pace * OVER_PACE_RATIO
        r["flagged"]   = r["spike"] or r["over_pace"]

    total     = sum(r["planned"] for r in rows) or 1
    backload  = sum(r["planned"] for r in rows[-2:]) / total if len(rows) >= 3 else 0
    remaining = [r for r in rows if r["end"] > as_of]
    triggered = any(r["flagged"] for r in rows) or backload > BACKLOAD_SHARE

    # ── Figure: planned vs delivered per sprint, on a date axis ──────────────
    # Each sprint gets two bars side by side inside its own date span, so
    # the increment boundary lines and Today sit at their true dates.
    durs      = [max((r["end"] - r["start"]).days, 1) for r in rows]
    starts    = [str(r["start"]) for r in rows]
    fig = go.Figure()
    fig.add_trace(go.Bar(
        x=starts, y=[r["planned"] for r in rows], name="Planned",
        width=[d * 0.40 * _DAY_MS for d in durs], offset=[d * 0.08 * _DAY_MS for d in durs],
        marker_color=[_OVER if r["flagged"] else _PLAN for r in rows],
        customdata=[r["label"] for r in rows],
        hovertemplate="Sprint %{customdata}: %{y:.0f} SP planned<extra></extra>",
    ))
    fig.add_trace(go.Bar(
        x=starts, y=[r["delivered"] for r in rows], name="Delivered",
        width=[d * 0.40 * _DAY_MS for d in durs], offset=[d * 0.52 * _DAY_MS for d in durs],
        marker_color=_DONE,
        customdata=[r["label"] for r in rows],
        hovertemplate="Sprint %{customdata}: %{y:.0f} SP delivered<extra></extra>",
    ))
    if pace:
        fig.add_hline(y=pace, line=dict(color=_MUTED, width=1.5, dash="dot"),
                      annotation_text=f"Delivered pace {pace:.0f} SP/sprint",
                      annotation_position="top right", annotation_font=dict(size=11, color=_MUTED))

    for r, d in zip(rows, durs):
        if not r["flagged"]:
            continue
        why = []
        if r["spike"]:
            why.append(f"{r['vs_prev']:.1f}× prior")
        if r["over_pace"]:
            why.append(f"{r['planned'] / pace:.1f}× pace")
        fig.add_annotation(x=str(datetime.combine(r["start"], datetime.min.time()) + timedelta(days=d * 0.28)),
                           y=r["planned"], text="<br>".join(why), showarrow=False,
                           yanchor="bottom", yshift=4, font=dict(size=11, color=_OVER))

    x_start = _window_start(inc, rows[0]["start"] - timedelta(days=7))
    x_end   = rows[-1]["end"] + timedelta(days=3)

    _base_layout(fig, height=440)
    fig.update_layout(barmode="overlay", hovermode="closest")
    fig.update_xaxes(
        range=[str(x_start), str(x_end)],
        tickvals=[str(datetime.combine(r["start"], datetime.min.time()) + timedelta(days=d / 2)) for r, d in zip(rows, durs)],
        ticktext=[f"{r['label']}<br>{_fmt_day(r['start'])}" for r in rows],
    )
    fig.update_yaxes(title=dict(text="Story points", font=dict(size=12, color=_MUTED)), rangemode="tozero")
    _increment_markers(fig, inc, x_start)
    if x_start <= as_of <= x_end:
        _today_line(fig, as_of)

    # ── Alert text ────────────────────────────────────────────────────────────
    flagged = [r for r in rows if r["flagged"]]
    lines = []
    if flagged:
        worst = max(flagged, key=lambda r: r["planned"] / pace if r["over_pace"] else (r["vs_prev"] or 0))
        parts = []
        if worst["spike"]:
            parts.append(f"{worst['vs_prev']:.1f}× the sprint before it")
        if worst["over_pace"]:
            parts.append(f"{worst['planned'] / pace:.1f}× the {pace:.0f} SP the team delivers per sprint")
        lines.append(f"Sprint {worst['label']} plans {worst['planned']:.0f} SP, " + " and ".join(parts) + ".")
        if len(flagged) > 1:
            others = ", ".join(r["label"] for r in flagged if r is not worst)
            lines.append(f"Also overloaded: {others}.")
    if backload > BACKLOAD_SHARE:
        lines.append(f"{backload:.0%} of the plan sits in the last two of {len(rows)} sprints.")
    if pace and remaining:
        left = sum(r["planned"] - (r["delivered"] or 0) for r in remaining)
        need = left / pace
        lines.append(f"The remaining sprints hold {left:.0f} SP of planned work. At the delivered pace "
                     f"that takes {need:.1f} sprints, and {len(remaining)} "
                     f"{'remains' if len(remaining) == 1 else 'remain'}.")
    elif not pace:
        lines.append("No sprint has finished yet, so there is no delivered pace to compare against.")
    if not lines:
        lines.append("Planned load stays close to what the team delivers, with no sharp jumps between sprints.")

    return {"figure": fig.to_plotly_json(), "alert": {
        "triggered": triggered,
        "headline":  "Sprint plan exceeds a sustainable pace" if triggered else "Sprint plan is within pace",
        "lines":     lines,
        "action":    ("Move scope out of the overloaded sprints or rebalance it across the PI "
                      "before the late sprints arrive.") if triggered else None,
        "status":    "Overloaded" if triggered else "Within pace",
    }}


# =============================================================================
# Public API
# =============================================================================

def build_alerts(combined_df: pl.DataFrame, sprint_plan: dict | None, as_of_date) -> dict:
    as_of = _to_date(as_of_date) or date.today()
    inc   = (sprint_plan or {}).get("increments")
    return {
        "wip":        _build_wip(combined_df, as_of, inc),
        "sprint":     _build_sprint_load(sprint_plan, combined_df, as_of, inc),
        "increments": inc,
    }


def rule_status(alert: dict | None):
    """Small status label for the rule list on the left."""
    if not alert:
        return html.Span("—", style={"color": _MUTED, "fontSize": "12px"})
    a = alert["alert"]
    color = _ALERT if a["triggered"] else _OK
    return html.Span(style={"display": "inline-flex", "alignItems": "center", "gap": "6px",
                            "fontSize": "12px", "color": color, "fontWeight": 600}, children=[
        html.Span(style={"width": "8px", "height": "8px", "borderRadius": "50%", "backgroundColor": color}),
        a["status"],
    ])


ALERT_CARD_BASE = {
    "backgroundColor": "#FFFFFF",
    "borderRadius": "12px",
    "padding": "16px 20px",
    "boxShadow": "0 2px 10px rgba(0,0,0,0.1)",
}


def render_alert_card(alert: dict | None):
    """Returns (children, style) for the alert card."""
    if not alert:
        return (html.P("Run Get Graph on the Projection Chart tab to check these rules.",
                       style={"margin": 0, "color": _MUTED}),
                {**ALERT_CARD_BASE, "borderLeft": f"4px solid {_RULE}"})

    a = alert["alert"]
    color = _ALERT if a["triggered"] else _OK
    children = [
        html.Div(style={"display": "flex", "alignItems": "center", "gap": "10px", "marginBottom": "8px"},
                 children=[
                     html.Span(style={"width": "10px", "height": "10px", "borderRadius": "50%",
                                      "backgroundColor": color, "flex": "none"}),
                     html.Span(a["headline"], style={"fontSize": "17px", "fontWeight": 600, "color": _TEXT}),
                 ]),
        *[html.P(line, style={"margin": "0 0 4px", "fontSize": "14px", "color": _TEXT}) for line in a["lines"]],
    ]
    if a.get("action"):
        children.append(html.P(a["action"], style={"margin": "8px 0 0", "fontSize": "13px", "color": _MUTED}))
    return children, {**ALERT_CARD_BASE, "borderLeft": f"4px solid {color}"}


# =============================================================================
# Carry-over notice
# =============================================================================

def _plural(n: int, word: str) -> str:
    return f"{n} {word}{'' if n == 1 else 's'}"


def render_carryover_notice(inc: dict | None):
    """
    Returns (children, style). Hidden unless issues carried over from an
    earlier increment or the prior increment's sprints overlap this one.
    """
    hidden = {"display": "none"}
    if not inc:
        return None, hidden

    carry   = inc.get("carryover") or []
    overlap = inc.get("overlap_days") or 0
    prior   = inc.get("prior")
    cur     = inc["current"]["label"]
    if not carry and not overlap:
        return None, hidden

    lines = []
    if carry:
        sources  = sorted({c["from_increment"] for c in carry})
        source   = sources[0] if len(sources) == 1 else "earlier increments"
        total_sp = sum(c["sp"] for c in carry)
        headline = f"{_plural(len(carry), 'issue')} ({total_sp:.0f} SP) carried over from {source}"
    else:
        headline = f"{prior['label']} overlaps {cur}"

    if overlap and prior:
        lines.append(f"{prior['label']}'s last sprint ran until {_fmt_day(_to_date(prior['end']))}, "
                     f"overlapping the first {_plural(overlap, 'day')} of {cur} (shaded on the charts).")

    if carry:
        replanned = [c for c in carry if c["replanned"]]
        unplanned = [c for c in carry if not c["replanned"]]
        still_open = [c for c in carry if not c["done_date"]]
        if replanned:
            lines.append(f"{len(replanned)} {'was' if len(replanned) == 1 else 'were'} re-planned into "
                         f"{cur} sprints and {'counts' if len(replanned) == 1 else 'count'} toward its baseline.")
        if unplanned:
            usp = sum(c["sp"] for c in unplanned)
            lines.append(f"{len(unplanned)} ({usp:.0f} SP) {'was' if len(unplanned) == 1 else 'were'} never "
                         f"re-planned, so {'it adds' if len(unplanned) == 1 else 'they add'} unplanned work that "
                         f"shows in the Actual lines but not the Baseline.")
        lines.append(f"{len(still_open)} {'is' if len(still_open) == 1 else 'are'} still open."
                     if still_open else "All of them have since been finished.")

    children = [
        html.Div(style={"display": "flex", "alignItems": "center", "gap": "10px", "marginBottom": "6px"},
                 children=[
                     html.Span(style={"width": "10px", "height": "10px", "borderRadius": "50%",
                                      "backgroundColor": _CARRY, "flex": "none"}),
                     html.Span(headline, style={"fontSize": "15px", "fontWeight": 600, "color": _TEXT}),
                 ]),
        *[html.P(t, style={"margin": "0 0 4px", "fontSize": "13px", "color": _TEXT}) for t in lines],
    ]

    if carry:
        cell = {"padding": "5px 10px 5px 0", "borderBottom": f"1px solid {_RULE}"}
        head = {**cell, "fontWeight": 500, "fontSize": "12px", "color": _MUTED, "textAlign": "left"}
        children.append(html.Details(style={"marginTop": "8px"}, children=[
            html.Summary(f"Show carried-over issues ({len(carry)})",
                         style={"cursor": "pointer", "fontSize": "13px", "fontWeight": 600}),
            html.Div(style={"overflowX": "auto", "marginTop": "6px"}, children=html.Table(
                style={"width": "100%", "borderCollapse": "collapse", "fontSize": "13px", "color": _TEXT},
                children=[
                    html.Thead(html.Tr([
                        html.Th("Issue", style=head), html.Th("Last earlier sprint", style=head),
                        html.Th("SP", style={**head, "textAlign": "right"}),
                        html.Th(f"In {cur} plan", style=head), html.Th("Status now", style=head),
                    ])),
                    html.Tbody([
                        html.Tr([
                            html.Td(c["key"], style={**cell, "whiteSpace": "nowrap"}),
                            html.Td(c["last_prior_sprint"], style={**cell, "color": _MUTED, "whiteSpace": "nowrap"}),
                            html.Td(f"{c['sp']:.0f}", style={**cell, "textAlign": "right",
                                                             "fontVariantNumeric": "tabular-nums"}),
                            html.Td("Yes" if c["replanned"] else "No",
                                    style={**cell, "color": _TEXT if c["replanned"] else _CARRY,
                                           "fontWeight": 400 if c["replanned"] else 600}),
                            html.Td(f"Done {_fmt_day(_to_date(c['done_date']))}" if c["done_date"] else c["state"],
                                    style={**cell, "color": _MUTED}),
                        ])
                        for c in carry
                    ]),
                ],
            )),
        ]))

    style = {**ALERT_CARD_BASE, "borderLeft": f"4px solid {_CARRY}"}
    return children, style
