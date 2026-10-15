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
# forecast_view.py
#
# Renders the forecast summary: a one-line verdict on the committed date,
# followed by three figures that back it up. Replaces the three separate
# mini cards above the chart (Tab 1) and the scenario cards (Tab 3).
# ---------------------------------------------------------------------------

from datetime import date, datetime

from dash import html

_TEXT  = "#1F2937"
_MUTED = "#6B7280"
_RULE  = "#E5E7EB"

# (minimum chance %, verdict, color)
_VERDICTS = [
    (85, "On track to finish by {d}",          "#15803D"),
    (50, "Likely to finish by {d}, with risk", "#B45309"),
    (15, "At risk of missing {d}",             "#C2410C"),
    (0,  "Unlikely to finish by {d}",          "#B91C1C"),
]

_NUM = {"fontVariantNumeric": "tabular-nums"}


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


def _fmt_chance(p: float) -> str:
    if p <= 0:
        return "0%"
    if p < 1:
        return "under 1%"
    if 99 < p < 100:
        return "over 99%"
    return f"{p:.0f}%"


def _relative(finish: date, target: date) -> str:
    days = (finish - target).days
    if days == 0:
        return f"on {_fmt_day(target)}"
    n, unit = (abs(days), "day") if abs(days) < 14 else (round(abs(days) / 7), "week")
    return f"{n} {unit}{'' if n == 1 else 's'} {'after' if days > 0 else 'before'} {_fmt_day(target)}"


def _stat(label, value, note, first=False):
    return html.Div(
        style={
            "padding": "0 20px" if not first else "0 20px 0 0",
            "borderLeft": None if first else f"1px solid {_RULE}",
            "minWidth": 0,
        },
        children=[
            html.Div(label, style={"fontSize": "12px", "color": _MUTED, "marginBottom": "4px"}),
            html.Div(value, style={**_NUM, "fontSize": "26px", "fontWeight": 600,
                                   "color": _TEXT, "lineHeight": 1.15}),
            html.Div(note, style={"fontSize": "12px", "color": _MUTED, "marginTop": "4px"}) if note else None,
        ],
    )


def render_forecast_summary(f: dict | None):
    """
    f keys (all optional):
        likelihood_pct   chance (0–100) of 100% by the committed date
        completion_pct   median % complete on the committed date
        finish_date      date the median projection reaches 100% (None if never)
        target_date      committed completion date
        median_peak      highest % the median projection reaches
        projection_count number of simulated runs
    """
    if not f:
        return html.P("Choose a team and PI, then select Get Graph to run the forecast.",
                      style={"margin": 0, "color": _MUTED})

    target     = _to_date(f.get("target_date"))
    finish     = _to_date(f.get("finish_date"))
    chance     = f.get("likelihood_pct")
    median_pct = f.get("completion_pct")
    peak       = f.get("median_peak")
    runs       = f.get("projection_count")
    target_txt = _fmt_day(target) if target else "the committed date"

    # ── Verdict line ──────────────────────────────────────────────────────────
    if chance is None or target is None:
        verdict, color = f"No forecast for {target_txt}", _MUTED
        detail = "The committed date falls outside the projection window."
    else:
        verdict, color = next((v.format(d=target_txt), c) for t, v, c in _VERDICTS if chance >= t)
        if runs:
            hits = round(chance / 100 * runs)
            who  = "None" if hits == 0 else ("All" if hits == runs else f"{hits:,}")
            detail = f"{who} of {runs:,} simulated runs finish by {target_txt}."
        else:
            detail = None

    verdict_row = html.Div(
        style={"display": "flex", "alignItems": "baseline", "gap": "10px",
               "flexWrap": "wrap", "marginBottom": "14px"},
        children=[
            html.Span(style={"width": "10px", "height": "10px", "borderRadius": "50%",
                             "backgroundColor": color, "flex": "none", "alignSelf": "center"}),
            html.Span(verdict, style={"fontSize": "17px", "fontWeight": 600, "color": _TEXT}),
            html.Span(detail, style={"fontSize": "13px", "color": _MUTED}) if detail else None,
        ],
    )

    # ── Supporting figures ────────────────────────────────────────────────────
    if finish is not None:
        finish_value = _fmt_day(finish)
        finish_note  = _relative(finish, target) if target else None
    elif peak is not None:
        finish_value = "Not reached"
        finish_note  = f"Median projection levels off at {peak:.0f}%"
    else:
        finish_value, finish_note = "—", None

    stats = html.Div(
        style={"display": "grid", "gridTemplateColumns": "repeat(auto-fit, minmax(170px, 1fr))",
               "rowGap": "12px"},
        children=[
            _stat(f"Chance to finish by {target_txt}",
                  _fmt_chance(chance) if chance is not None else "—",
                  None, first=True),
            _stat("Median finish date", finish_value, finish_note),
            _stat(f"Median progress on {target_txt}",
                  f"{median_pct:.0f}%" if median_pct is not None else "—",
                  "of in-scope story points done"),
        ],
    )

    return html.Div([verdict_row, stats])


# ---------------------------------------------------------------------------
# Scenario cards (Tab 3)
# ---------------------------------------------------------------------------

def render_scenario_card(index: int, label: str, f: dict | None):
    """
    A forecast summary headed by the scenario's name, with a swatch that
    matches that scenario's line and band on the comparison chart.
    """
    from utils.figures import SCENARIO_STYLES   # local import keeps this module light
    style = SCENARIO_STYLES[index % len(SCENARIO_STYLES)]
    header = html.Div(
        style={"display": "flex", "alignItems": "center", "gap": "8px", "marginBottom": "12px"},
        children=[
            html.Span(style={
                "width": "22px", "height": "0", "flex": "none",
                "borderTop": f"3px {'dotted' if style['dash'] == 'dot' else 'dashed'} {style['color']}",
            }),
            html.Span(label, style={"fontSize": "13px", "fontWeight": 600, "color": style["color"]}),
        ],
    )
    return [header, render_forecast_summary(f)]
