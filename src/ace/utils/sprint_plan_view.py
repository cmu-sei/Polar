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
# sprint_plan_view.py
#
# Renders the sprint_plan dict returned by build_pi_baseline() as a compact
# table for the "Sprint Plan" card. Share bars use the Baseline line color so
# the table reads as a legend for the red line in the chart above it.
# ---------------------------------------------------------------------------

import re
from datetime import date

from dash import html

_TEXT      = "#1F2937"
_MUTED     = "#6B7280"
_RULE      = "#E5E7EB"
_BAR       = "#D16D6A"   # matches COLORS["Baseline"] in figures.py
_BAR_TRACK = "#F3F4F6"
_NOW_BG    = "#F1F5F9"
_NOW_EDGE  = "#94A3B8"   # matches COLORS["today_line"]
_SPIKE     = "#B45309"

_NUM = {"textAlign": "right", "fontVariantNumeric": "tabular-nums", "whiteSpace": "nowrap"}


def _d(iso: str) -> date:
    return date.fromisoformat(iso[:10])


def _fmt_day(d: date) -> str:
    return f"{d:%b} {d.day}"


def _fmt_range(start_iso: str, end_iso: str) -> str:
    return f"{_fmt_day(_d(start_iso))} – {_fmt_day(_d(end_iso))}"


def _parse_today(todays_date) -> date | None:
    if not todays_date:
        return None
    try:
        return date.fromisoformat(str(todays_date)[:10])
    except ValueError:
        return None


def _cell(children, style=None):
    base = {"padding": "7px 8px", "borderBottom": f"1px solid {_RULE}", "verticalAlign": "middle"}
    return html.Td(children, style={**base, **(style or {})})


def _head(text, align="left"):
    return html.Th(text, style={
        "padding": "0 8px 6px", "textAlign": align, "fontWeight": 500,
        "fontSize": "12px", "color": _MUTED, "borderBottom": f"1px solid {_RULE}",
        "whiteSpace": "nowrap",
    })


def _share_bar(pct: float, max_pct: float):
    width = 0 if max_pct <= 0 else pct / max_pct * 100
    return html.Div(style={"display": "flex", "alignItems": "center", "gap": "8px"}, children=[
        html.Div(style={
            "flex": "1", "minWidth": "60px", "height": "6px",
            "backgroundColor": _BAR_TRACK, "borderRadius": "3px", "overflow": "hidden",
        }, children=html.Div(style={
            "width": f"{width:.1f}%", "height": "100%", "backgroundColor": _BAR,
        })),
        html.Span(f"{pct:.1f}%", style={**_NUM, "width": "44px", "color": _MUTED}),
    ])


def _sprint_label(row: dict):
    label = re.sub(r"\s*\(?\bIP\b\)?", "", row["label"]).strip() if row["is_ip"] else row["label"]
    parts = [html.Span(label or row["label"], style={"fontWeight": 600})]
    if row["is_ip"]:
        parts.append(html.Span("IP", title="Innovation & Planning sprint", style={
            "marginLeft": "6px", "fontSize": "11px", "padding": "1px 6px",
            "borderRadius": "4px", "border": f"1px solid {_RULE}", "color": _MUTED,
        }))
    if row["spike"]:
        parts.append(html.Span("▲", title="Planned SP up more than 50% from the previous sprint",
                               style={"marginLeft": "6px", "fontSize": "11px", "color": _SPIKE}))
    return parts


def render_sprint_plan(plan: dict | None, todays_date=None):
    """Return Dash children for the Sprint Plan card."""
    if not plan or not plan.get("sprints"):
        return html.P(
            "No sprints found for this team and PI. Check that sprint names include the PI label.",
            style={"color": _MUTED, "margin": 0},
        )

    sprints = plan["sprints"]
    today   = _parse_today(todays_date)
    max_pct = max(s["pct"] for s in sprints)
    n       = len(sprints)

    summary = html.P(
        f"{plan['total_sp']:.0f} SP planned across {n} sprint{'s' if n != 1 else ''}, "
        f"{_fmt_day(_d(plan['pi_start']))} to {_fmt_day(_d(plan['pi_end']))}.",
        style={"margin": "0 0 12px", "color": _TEXT},
    )

    rows = []
    for s in sprints:
        is_now = today is not None and _d(s["start"]) <= today < _d(s["end"])
        row_style = {"backgroundColor": _NOW_BG, "boxShadow": f"inset 3px 0 0 {_NOW_EDGE}"} if is_now else {}
        rows.append(html.Tr(style=row_style, title="Current sprint" if is_now else None, children=[
            _cell(_sprint_label(s), {"whiteSpace": "nowrap"}),
            _cell(_fmt_range(s["start"], s["end"]), {"color": _MUTED, "whiteSpace": "nowrap"}),
            _cell(f"{s['sp']:.0f}", _NUM),
            _cell(_share_bar(s["pct"], max_pct), {"width": "40%"}),
            _cell(f"{s['sp_per_week']:.1f}", _NUM),
        ]))

    table = html.Div(style={"overflowX": "auto"}, children=html.Table(
        style={"width": "100%", "borderCollapse": "collapse", "fontSize": "13px", "color": _TEXT},
        children=[
            html.Thead(html.Tr([
                _head("Sprint"), _head("Dates"), _head("SP", "right"),
                _head("Share of plan"), _head("SP / week", "right"),
            ])),
            html.Tbody(rows),
        ],
    ))

    notes = []
    if plan.get("carryover_sp", 0) > 0:
        c = plan["carryover_count"]
        notes.append(
            f"{plan['carryover_sp']:.0f} SP in {c} issue{'s' if c != 1 else ''} "
            "sits only in other PIs' sprints. It counts toward actuals, not the baseline."
        )
    if plan.get("unassigned_sp", 0) > 0:
        c = plan["unassigned_count"]
        notes.append(
            f"{plan['unassigned_sp']:.0f} SP in {c} issue{'s' if c != 1 else ''} "
            "has no sprint. It counts toward actuals, not the baseline."
        )

    footer = html.Div(
        [html.P(t, style={"margin": "4px 0 0"}) for t in notes],
        style={"marginTop": "12px", "fontSize": "12px", "color": _MUTED},
    ) if notes else None

    return html.Div([summary, table, footer])
