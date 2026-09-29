# ---------------------------------------------------------------------------
# status_view.py
#
# Renders the dict from build_status_summary() for the "Status of remaining
# issues" card. Colors match the chart: Actual Done, Actual In Progress and
# Baseline use the same hues as their lines.
# ---------------------------------------------------------------------------

from datetime import date

from dash import html

_TEXT       = "#1F2937"
_MUTED      = "#6B7280"
_RULE       = "#E5E7EB"
_DONE       = "#1D3557"   # COLORS["Actual_Completed"]
_IN_PROG    = "#4F7CAC"   # COLORS["Actual_In_Progress"]
_PLAN       = "#D16D6A"   # COLORS["Baseline"]
_NOT_START  = "#CBD5E1"
_PARKED     = "repeating-linear-gradient(135deg, #E5E7EB 0 4px, #F9FAFB 4px 8px)"
_TRACK      = "#F3F4F6"
_STALE      = "#B45309"
STALE_DAYS  = 14

_NUM = {"textAlign": "right", "fontVariantNumeric": "tabular-nums", "whiteSpace": "nowrap"}


def _fmt_day(iso: str) -> str:
    d = date.fromisoformat(iso[:10])
    return f"{d:%b} {d.day}"


def _pct(part: float, whole: float) -> str:
    return f"{part / whole * 100:.0f}%" if whole else "0%"


def _plural(n: int, word: str) -> str:
    return f"{n} {word}{'' if n == 1 else 's'}"


def _subhead(text):
    return html.H6(text, style={"margin": "18px 0 6px", "fontWeight": 600, "color": _TEXT})


def _swatch(bg):
    return html.Span(style={
        "display": "inline-block", "width": "10px", "height": "10px",
        "borderRadius": "2px", "background": bg, "marginRight": "6px",
        "verticalAlign": "middle", "border": f"1px solid {_RULE}",
    })


# ── 1. Where the work stands ──────────────────────────────────────────────────

def _stacked_bar(s: dict):
    total = s["total_sp"] or 1
    parts = [
        ("Done",        s["buckets"]["done"],        _DONE),
        ("In progress", s["buckets"]["in_progress"], _IN_PROG),
        ("Not started", s["buckets"]["not_started"], _NOT_START),
        ("Parked",      s["buckets"]["parked"],      _PARKED),
    ]
    bar = html.Div(
        style={"display": "flex", "height": "10px", "borderRadius": "5px",
               "overflow": "hidden", "backgroundColor": _TRACK},
        children=[
            html.Div(title=f"{label}: {b['sp']:.0f} SP",
                     style={"width": f"{b['sp'] / total * 100:.2f}%", "background": color})
            for label, b, color in parts if b["sp"] > 0
        ],
    )
    legend = html.Div(
        style={"display": "flex", "flexWrap": "wrap", "gap": "4px 16px",
               "marginTop": "8px", "fontSize": "12px", "color": _MUTED},
        children=[
            html.Span([_swatch(color), f"{label} ",
                       html.Span(f"{b['sp']:.0f} SP", style={"color": _TEXT, "fontWeight": 600}),
                       f" in {_plural(b['count'], 'issue')}"])
            for label, b, color in parts if b["count"] > 0
        ],
    )
    return html.Div([bar, legend])


# ── 2. Why In Progress tops out below 100% ────────────────────────────────────

def _in_progress_gap(s: dict):
    total      = s["total_sp"]
    unstarted  = total - s["started_sp"]
    if unstarted <= 0:
        return None

    rows = [
        (f"Not started, {r['state']}", r["sp"], r["count"])
        for r in s["unstarted_by_state"]
    ]
    dws = s["done_without_start"]
    if dws["count"]:
        rows.append(("Done without passing through In Progress", dws["sp"], dws["count"]))

    table = html.Table(
        style={"width": "100%", "borderCollapse": "collapse", "fontSize": "13px"},
        children=html.Tbody([
            html.Tr([
                html.Td(label, style={"padding": "4px 0", "borderBottom": f"1px solid {_RULE}"}),
                html.Td(f"{sp:.0f} SP", style={**_NUM, "padding": "4px 8px", "borderBottom": f"1px solid {_RULE}"}),
                html.Td(_plural(count, "issue"), style={**_NUM, "padding": "4px 0", "color": _MUTED,
                                                         "borderBottom": f"1px solid {_RULE}"}),
            ])
            for label, sp, count in rows
        ]),
    )

    return html.Div([
        _subhead(f"Why In Progress stops at {_pct(s['started_sp'], total)}"),
        html.P(
            f"The In Progress line counts story points that have entered the In Progress "
            f"state at least once. {unstarted:.0f} SP ({_pct(unstarted, total)}) never have:",
            style={"margin": "0 0 6px", "fontSize": "13px"},
        ),
        table,
        html.P(
            "Work that moves straight from To Do to another active or done state is never "
            "counted as started, so it widens this gap even when the work is moving.",
            style={"margin": "6px 0 0", "fontSize": "12px", "color": _MUTED},
        ) if dws["count"] else None,
    ])


# ── 3. Against the plan ───────────────────────────────────────────────────────

def _plan_row(label, sp, scale, color):
    width = min(sp / scale * 100, 100) if scale else 0
    return html.Div(
        style={"display": "grid", "gridTemplateColumns": "130px 1fr 60px",
               "alignItems": "center", "gap": "8px", "fontSize": "13px", "margin": "4px 0"},
        children=[
            html.Span(label, style={"color": _MUTED}),
            html.Div(style={"height": "8px", "backgroundColor": _TRACK, "borderRadius": "4px", "overflow": "hidden"},
                     children=html.Div(style={"width": f"{width:.1f}%", "height": "100%", "backgroundColor": color})),
            html.Span(f"{sp:.0f} SP", style=_NUM),
        ],
    )


def _gap_phrase(actual, expected, what):
    diff = actual - expected
    if abs(diff) < 0.5:
        return f"{what} is on plan"
    return f"{what} {'leads' if diff > 0 else 'trails'} the plan by {abs(diff):.0f} SP"


def _plan_comparison(s: dict):
    plan     = s["plan"]
    expected = plan["expected_sp"]
    started  = s["started_sp"]
    done     = s["buckets"]["done"]["sp"]
    scale    = max(plan["planned_total"], s["total_sp"], 1)

    children = [
        _subhead(f"Against the plan on {_fmt_day(s['as_of'])}"),
        _plan_row("Plan expected done", expected, scale, _PLAN),
        _plan_row("Started",            started,  scale, _IN_PROG),
        _plan_row("Done",               done,     scale, _DONE),
        html.P(
            f"{_gap_phrase(started, expected, 'Started work')}; "
            f"{_gap_phrase(done, expected, 'finished work').lower()}.",
            style={"margin": "6px 0 0", "fontSize": "13px"},
        ),
    ]

    scope = s["scope"]
    if abs(plan["planned_total"] - s["total_sp"]) >= 0.5 or scope["baseline_only_count"] or scope["actual_only_count"]:
        parts = [
            f"The plan covers {plan['planned_total']:.0f} SP and the actual lines cover "
            f"{s['total_sp']:.0f} SP, so their percentages on the chart use different totals."
        ]
        if scope["baseline_only_count"]:
            parts.append(f"{_plural(scope['baseline_only_count'], 'issue')} "
                         f"({scope['baseline_only_sp']:.0f} SP) are only in the plan.")
        if scope["actual_only_count"]:
            parts.append(f"{_plural(scope['actual_only_count'], 'issue')} "
                         f"({scope['actual_only_sp']:.0f} SP) are only in the actuals.")
        if scope["sp_changed_count"]:
            parts.append(f"{_plural(scope['sp_changed_count'], 'issue')} changed estimate.")
        children.append(html.P(" ".join(parts), style={"margin": "6px 0 0", "fontSize": "12px", "color": _MUTED}))

    return html.Div(children)


# ── 4. Open issues list ───────────────────────────────────────────────────────

def _open_issues(s: dict):
    issues = s["open_issues"]
    if not issues:
        return None

    def th(text, right=False):
        return html.Th(text, style={"padding": "0 8px 6px 0", "fontWeight": 500, "fontSize": "12px",
                                    "color": _MUTED, "textAlign": "right" if right else "left",
                                    "borderBottom": f"1px solid {_RULE}"})

    def row(r):
        days  = r["days_in_state"]
        stale = r["started"] and days is not None and days > STALE_DAYS
        cell  = {"padding": "5px 8px 5px 0", "borderBottom": f"1px solid {_RULE}"}
        return html.Tr([
            html.Td([_swatch(_IN_PROG if r["started"] else _NOT_START), r["key"]],
                    style={**cell, "whiteSpace": "nowrap"}),
            html.Td(r["state"], style={**cell, "color": _MUTED}),
            html.Td(f"{r['sp']:.0f}", style={**cell, **_NUM}),
            html.Td("—" if days is None else f"{days}",
                    title=f"In this state more than {STALE_DAYS} days" if stale else None,
                    style={**cell, **_NUM, "color": _STALE if stale else _TEXT,
                           "fontWeight": 600 if stale else 400}),
        ])

    stale_count = sum(1 for r in issues if r["started"] and (r["days_in_state"] or 0) > STALE_DAYS)
    summary_text = f"Open issues ({len(issues)})"
    if stale_count:
        summary_text += f", {stale_count} in progress for over {STALE_DAYS} days"

    return html.Details(style={"marginTop": "18px"}, children=[
        html.Summary(summary_text, style={"cursor": "pointer", "fontWeight": 600, "fontSize": "14px"}),
        html.Div(style={"overflowX": "auto", "marginTop": "8px"}, children=html.Table(
            style={"width": "100%", "borderCollapse": "collapse", "fontSize": "13px", "color": _TEXT},
            children=[
                html.Thead(html.Tr([th("Issue"), th("State"), th("SP", True), th("Days in state", True)])),
                html.Tbody([row(r) for r in issues]),
            ],
        )),
    ])


# ── Public ────────────────────────────────────────────────────────────────────

def render_status_summary(s: dict | None):
    if not s or not s.get("total_issues"):
        return html.P("No issues found for this team and PI.", style={"color": _MUTED, "margin": 0})

    open_count = s["buckets"]["in_progress"]["count"] + s["buckets"]["not_started"]["count"] + s["buckets"]["parked"]["count"]
    open_sp    = s["total_sp"] - s["buckets"]["done"]["sp"]

    headline = html.P(
        f"As of {_fmt_day(s['as_of'])}, {open_count} of {s['total_issues']} issues are open, "
        f"holding {open_sp:.0f} of {s['total_sp']:.0f} SP. "
        f"{s['buckets']['in_progress']['count']} of them are in progress.",
        style={"margin": "0 0 10px", "color": _TEXT},
    )

    return html.Div(style={"fontSize": "13px", "color": _TEXT}, children=[
        headline,
        _stacked_bar(s),
        _in_progress_gap(s),
        _plan_comparison(s),
        _open_issues(s),
    ])
