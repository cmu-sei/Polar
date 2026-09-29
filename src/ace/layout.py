from dash import html, dcc
from config import app_color
import dash_bootstrap_components as dbc

from utils.forecast_view import render_forecast_summary


# =============================================================================
# Form helpers: every input panel uses these, so fields look and behave alike.
# Control sizing, borders and focus rings for dcc.Dropdown / DatePickerSingle /
# Input live in assets/forms.css (those components can't be fully styled inline).
# =============================================================================

_LABEL = {
    "display": "block",
    "fontSize": "13px",
    "fontWeight": 500,
    "color": "#374151",
    "marginBottom": "6px",
}

_HELP = {
    "fontSize": "12px",
    "color": "#6B7280",
    "margin": "6px 0 0",
    "lineHeight": 1.4,
}

_SECTION_TITLE = {
    "fontSize": "12px",
    "fontWeight": 600,
    "color": "#6B7280",
}

_PANEL = {
    "flex": "2 1 0",
    "minWidth": "280px",
    "alignSelf": "flex-start",
    "backgroundColor": "#FFFFFF",
    "borderRadius": "12px",
    "padding": "20px",
    "boxShadow": "0 2px 10px rgba(0,0,0,0.08)",
    "display": "flex",
    "flexDirection": "column",
    "gap": "16px",
}

_PANEL_TITLE = {"fontSize": "15px", "fontWeight": 600, "color": "#1F2937", "margin": 0}


def _field(label, control_id, control, help_text=None):
    """Label above the control, optional help text below it."""
    return html.Div(style={"minWidth": 0}, children=[
        html.Label(label, htmlFor=control_id, style=_LABEL),
        control,
        html.P(help_text, style=_HELP) if help_text else None,
    ])


def _section(title, children, first=False):
    """A titled group of fields, separated from the one above by a rule."""
    return html.Div(
        style={
            "display": "flex", "flexDirection": "column", "gap": "14px",
            "paddingTop": "0" if first else "16px",
            "borderTop": None if first else "1px solid #E5E7EB",
        },
        children=[html.Div(title, style=_SECTION_TITLE), *children],
    )


def _date_picker(control_id, date):
    return dcc.DatePickerSingle(
        id=control_id,
        date=date,
        display_format="MMM D, YYYY",
        placeholder="Select a date",
        className="form-date",
    )


def _primary_button(text, control_id):
    return html.Button(
        text, id=control_id, n_clicks=0, className="form-button-primary",
        style={
            "width": "100%", "height": "40px", "marginTop": "4px",
            "backgroundColor": app_color["accent"], "color": "#FFFFFF",
            "border": "none", "borderRadius": "6px",
            "fontSize": "14px", "fontWeight": 600, "cursor": "pointer",
        },
    )


def create_layout(pi_training_options, team_options, pi_display_options):

    card_style = {
        "backgroundColor": "#FFFFFF",
        "padding": "20px",
        "borderRadius": "12px",
        "marginBottom": "25px",
        "boxShadow": "0 2px 10px rgba(0,0,0,0.08)"
    }





    tab_style = {
        "flex": "1",
        "border": "none",
        "backgroundColor": "transparent",
        "color": "#6B7280",
        "padding": "10px 20px",
        "textAlign": "center",
        "borderRadius": "20px",
        "margin": "4px",
        "transition": "all 0.2s ease",
    }

    tab_selected_style = {
        "flex": "1",
        "border": "none",
        "backgroundColor": "#FFFFFF",
        "color": "black",
        "padding": "10px 20px",
        "textAlign": "center",
        "borderRadius": "20px",
        "margin": "4px",
        "boxShadow": "0 2px 4px rgba(59, 130, 246, 0.3)",
        "transition": "all 0.2s ease",
    }

    selected_style = {
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
        "boxShadow": "0 2px 4px rgba(0,0,0,0.1)",
    }

    unselected_style = {
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


    forecast_card_style = {
        "backgroundColor": "#FFFFFF",
        "borderRadius": "12px",
        "padding": "16px 20px",
        "boxShadow": "0 2px 10px rgba(0,0,0,0.1)",
        "textAlign": "left",
    }

    scenario_label_style = {
        "fontSize": "12px",
        "color": "#6B7280",
        "marginBottom": "10px",
    }

    graph_card_style = {
        "backgroundColor": "#FFFFFF",
        "borderRadius": "12px",
        "padding": "12px",
        "boxShadow": "0 2px 10px rgba(0,0,0,0.1)",
    }

    layout = html.Div(
        style={"backgroundColor": app_color["background"], "minHeight": "100vh", "fontFamily": "Inter, Segoe UI, sans-serif"},
        children=[

            # ── Top header bar ────────────────────────────────────────────
            html.Div(
                style={
                    "display": "flex",
                    "justifyContent": "space-between",
                    "alignItems": "center",
                    "padding": "16px 40px",
                    "backgroundColor": "#FFFFFF",
                    "borderBottom": "1px solid #E5E7EB",
                    "boxShadow": "0 1px 4px rgba(0,0,0,0.06)",
                    "marginBottom": "32px",
                },
                children=[
                    # Left: accent bar + title + tagline
                    html.Div(
                        style={"display": "flex", "alignItems": "center", "gap": "12px"},
                        children=[
                            html.Div(style={
                                "width": "4px", "height": "32px",
                                "backgroundColor": "#3B82F6",
                                "borderRadius": "2px",
                            }),
                            html.Div([
                                html.H2("Team Projection Dashboard",
                                    style={
                                        "margin": 0,
                                        "fontWeight": 600,
                                        "fontSize": "20px",
                                        "color": "#1F2937",
                                        "letterSpacing": "-0.3px",
                                    }),
                                html.P("Sprint velocity & burnup analytics",
                                    style={
                                        "margin": 0,
                                        "fontSize": "13px",
                                        "color": "#6B7280",
                                        "fontWeight": 400,
                                    })
                            ])
                        ]
                    ),
                ]
            ),

            # ── Main content ──────────────────────────────────────────────
            html.Div(style={"padding": "0 40px 40px 40px"}, children=[

            html.Div([
                dcc.Tabs(
                    id='pi-tabs',
                    value='projection',
                    style={
                        "backgroundColor": "#E5E9F0",
                        "borderRadius": "24px",
                        "padding": "4px",
                        "display": "flex",
                        "marginBottom": "20px",
                    },
                    colors={
                        "border": "transparent",
                        "primary": "transparent",
                        "background": "transparent"
                    },
                    children=[

                        # =====================================================
                        # TAB 1: Projection Chart
                        # =====================================================
                        dcc.Tab(
                            label="Projection Chart",
                            value="projection",
                            style=tab_style,
                            selected_style=tab_selected_style,
                            children=[
                                html.Div([

                                    # LEFT SIDE: forecast settings
                                    html.Div(style=_PANEL, children=[
                                        html.H5("Forecast settings", style=_PANEL_TITLE),

                                        _section("Scope", first=True, children=[
                                            _field("Team", "team-dropdown", dcc.Dropdown(
                                                id="team-dropdown",
                                                options=[{"label": t, "value": t} for t in team_options],
                                                value=team_options[-3] if len(team_options) >= 3 else (team_options[0] if team_options else None),
                                                clearable=False,
                                                placeholder="Select a team",
                                                className="form-dropdown",
                                            )),
                                            _field("Program increment", "pi-display-dropdown", dcc.Dropdown(
                                                id="pi-display-dropdown",
                                                options=pi_display_options,
                                                value=None,
                                                clearable=False,
                                                placeholder="Select a PI",
                                                className="form-dropdown",
                                            )),
                                            dcc.Interval(id="team-refresh-interval", interval=5 * 60 * 1000, n_intervals=0),
                                        ]),

                                        _section("Dates", children=[
                                            html.Div(
                                                style={"display": "grid",
                                                       "gridTemplateColumns": "repeat(auto-fit, minmax(140px, 1fr))",
                                                       "gap": "14px"},
                                                children=[
                                                    _field("PI start", "sprint-startdate-input",
                                                           _date_picker("sprint-startdate-input", "2024-01-05")),
                                                    _field("Committed completion", "estimated-project-completion-date",
                                                           _date_picker("estimated-project-completion-date", "2025-05-28")),
                                                ],
                                            ),
                                            _field("Projection start", "todays-date",
                                                   _date_picker("todays-date", "2026-04-01"),
                                                   help_text="The forecast branches from actual progress on this date."),
                                            html.Div(id="todays-date-output"),
                                        ]),

                                        # Advanced settings (toggled by toggle_collapse in callbacks.py)
                                        html.Div(style={"paddingTop": "16px", "borderTop": "1px solid #E5E7EB"}, children=[
                                            html.Button(
                                                [html.Span("Advanced settings"), html.Span("▾", style={"fontSize": "11px"})],
                                                id="advanced-button", n_clicks=0, className="form-disclosure",
                                            ),
                                            dbc.Collapse(id="advanced-collapse", is_open=False, children=html.Div(
                                                style={"display": "flex", "flexDirection": "column", "gap": "14px", "paddingTop": "14px"},
                                                children=[
                                                    _field("Issue types", "issue_type_selection", dcc.Dropdown(
                                                        id="issue_type_selection",
                                                        options=[
                                                            {"label": "Stories only",    "value": "Just Stories"},
                                                            {"label": "All issue types", "value": "All Issues"},
                                                        ],
                                                        value="Just Stories",
                                                        clearable=False,
                                                        searchable=False,
                                                        className="form-dropdown",
                                                    )),
                                                    _field("Team size", "team_size", dcc.Input(
                                                        id="team_size", type="number", value=6, min=1, step=1,
                                                        className="form-input",
                                                    ), help_text="People available to work the remaining issues."),
                                                    _field("Training PIs", "pi-training-dropdown", dcc.Dropdown(
                                                        id="pi-training-dropdown",
                                                        options=pi_training_options,
                                                        value=[],
                                                        multi=True,
                                                        placeholder="Select past PIs",
                                                        className="form-dropdown",
                                                    ), help_text="Past PIs used to estimate velocity. Defaults to the two before the selected PI."),
                                                ],
                                            )),
                                        ]),

                                        _primary_button("Run forecast", "generate-graph-projections-btn"),

                                        dcc.Store(id="stored-projection-data"),
                                        dcc.Store(id="pi-data-all-issues"),
                                    ]),

                                    # RIGHT SIDE
                                    html.Div(
                                        style={
                                            "flex": "5 1 0",
                                            "minWidth": "300px",
                                            "paddingLeft": "16px",
                                            "display": "flex",
                                            "flexDirection": "column",
                                            "gap": "12px"
                                        },
                                        children=[

                                            # Forecast summary (verdict + three figures)
                                            html.Div(
                                                id="forecast-summary",
                                                style=forecast_card_style,
                                                children=render_forecast_summary(None),
                                            ),

                                            # Main chart
                                            html.Div(
                                                id="graph-container-projection",
                                                style=graph_card_style,
                                                children=[
                                                    dcc.Loading(
                                                        id="graph-loading",
                                                        type="default",
                                                        color="#3B82F6",
                                                        children=dcc.Graph(id="progress-graph-projection", style={"minHeight": 380})
                                                    )
                                                ]
                                            ),

                                            # Bottom cards: remaining issues + sprint plan
                                            html.Div(
                                                style={
                                                    "flex": "1",
                                                    "display": "grid",
                                                    "gridTemplateColumns": "1fr 1.3fr",
                                                    "gridTemplateRows": "1fr",
                                                    "gap": "8px",
                                                },
                                                children=[
                                                    html.Div(
                                                        id="incomplete_issue_display",
                                                        style={
                                                            "backgroundColor": "#FFFFFF",
                                                            "borderRadius": "8px",
                                                            "padding": "12px 16px",
                                                            "boxShadow": "0 2px 6px rgba(0,0,0,0.1)",
                                                            "textAlign": "left",
                                                            "minWidth": 0,
                                                        },
                                                        children=[
                                                            html.H5("Status of remaining issues", style={"marginBottom": "8px"}),
                                                            html.Div(id="remaining_issue_display", style={"fontSize": "13px"}),
                                                        ]
                                                    ),
                                                    html.Div(
                                                        id="sprint_plan_display",
                                                        style={
                                                            "backgroundColor": "#FFFFFF",
                                                            "borderRadius": "8px",
                                                            "padding": "12px 16px",
                                                            "boxShadow": "0 2px 6px rgba(0,0,0,0.1)",
                                                            "textAlign": "left",
                                                            "minWidth": 0,   # lets the table scroll inside the cell
                                                        },
                                                        children=[
                                                            html.H5("Sprint Plan", style={"marginBottom": "8px"}),
                                                            html.Div(id="sprint_plan_display_string", style={"fontSize": "13px"}),
                                                        ]
                                                    ),
                                                ]
                                            ),
                                        ]
                                    )

                                ], style={"display": "flex", "gap": "16px", "alignItems": "flex-start", "border": "none"})
                            ]
                        ),

                        # =====================================================
                        # TAB 2: Realtime Alerts
                        # =====================================================
                        dcc.Tab(
                            label="Realtime Alerts",
                            value="tab-2",
                            style=tab_style,
                            selected_style=tab_selected_style,
                            children=[

                                dcc.Store(id="selected-option", data=0),
                                dcc.Store(id="alerts-store"),
                                dcc.Store(id="scenario1-store"),

                                html.Div(
                                    style={
                                        "display": "flex",
                                        "flexDirection": "row",
                                        "gap": "16px",
                                        "alignItems": "flex-start",
                                    },
                                    children=[

                                        # LEFT SIDE: rule list with live status
                                        html.Div(
                                            style={
                                                "flex": "2 1 0",
                                                "minWidth": "260px",
                                                "paddingLeft": "16px",
                                                "display": "flex",
                                                "flexDirection": "column",
                                                "gap": "8px",
                                            },
                                            children=[
                                                html.H5("Alert rules", style={"margin": "0", "color": "#1F2937"}),
                                                html.P(
                                                    "Checked as of the Projection Start Date each time you run Get Graph.",
                                                    style={"margin": "0 0 4px", "fontSize": "13px", "color": "#6B7280"},
                                                ),
                                                html.Div(
                                                    id="vertical-selector",
                                                    style={
                                                        "backgroundColor": "#E5E9F0",
                                                        "borderRadius": "12px",
                                                        "padding": "4px",
                                                        "display": "flex",
                                                        "flexDirection": "column",
                                                        "gap": "4px",
                                                    },
                                                    children=[
                                                        html.Button(
                                                            [html.Span("Work in progress"), html.Span(id="rule-status-wip")],
                                                            id={"type": "v-option", "index": 0}, style=selected_style,
                                                        ),
                                                        html.Button(
                                                            [html.Span("Sprint load"), html.Span(id="rule-status-sprint")],
                                                            id={"type": "v-option", "index": 1}, style=unselected_style,
                                                        ),
                                                    ]
                                                ),
                                            ]
                                        ),

                                        # RIGHT SIDE: alert message + supporting chart
                                        html.Div(
                                            style={
                                                "display": "flex",
                                                "flexDirection": "column",
                                                "gap": "16px",
                                                "flex": "5 1 0",
                                                "minWidth": "300px",
                                                "paddingLeft": "16px",
                                            },
                                            children=[
                                                html.Div(id="alert-card"),
                                                html.Div(
                                                    id="graph-container-projection-tab2",
                                                    style=graph_card_style,
                                                    children=[
                                                        dcc.Loading(
                                                            id="graph-loading-tab2",
                                                            type="default",
                                                            color="#3B82F6",
                                                            children=dcc.Graph(id="progress-graph-projection-tab2", style={"minHeight": 420})
                                                        )
                                                    ]
                                                ),
                                                # Shown only when work carried over from an earlier increment
                                                html.Div(id="carryover-notice", style={"display": "none"}),
                                            ]
                                        ),
                                    ]
                                )
                            ]
                        ),

                        # =====================================================
                        # TAB 3: Team Impact Adjustment
                        # =====================================================
                        dcc.Tab(
                            label="Team Impact Adjustment",
                            value="tab-3",
                            style=tab_style,
                            selected_style=tab_selected_style,
                            children=[
                                html.Div([

                                    # LEFT SIDE: scenario settings
                                    html.Div(style=_PANEL, children=[
                                        html.H5("Scenario settings", style=_PANEL_TITLE),
                                        _section("Scenario 2", first=True, children=[
                                            _field("Team size", "team_size-tab3", dcc.Input(
                                                id="team_size-tab3", type="number", value=6, min=1, step=1,
                                                className="form-input",
                                            ), help_text="Scenario 1 uses the team size from the Projection Chart tab. "
                                                         "Team, PI and dates also come from that tab."),
                                        ]),
                                        _primary_button("Run comparison", "generate-graph-projections-btn-tab3"),
                                    ]),

                                    # RIGHT SIDE
                                    html.Div([
                                        html.H4("Scenario comparison"),
                                        html.Div(
                                            style={
                                                "display": "grid",
                                                "gridTemplateColumns": "repeat(auto-fit, minmax(320px, 1fr))",
                                                "gap": "12px",
                                                "marginBottom": "16px",
                                            },
                                            children=[
                                                html.Div(style=forecast_card_style, children=html.Div(
                                                    id="forecast-summary-tab3-team1",
                                                    children=[html.Div("Scenario 1: team size from the Projection Chart tab", style=scenario_label_style),
                                                              render_forecast_summary(None)],
                                                )),
                                                html.Div(style=forecast_card_style, children=html.Div(
                                                    id="forecast-summary-tab3-team2",
                                                    children=[html.Div("Scenario 2: team size set here", style=scenario_label_style),
                                                              render_forecast_summary(None)],
                                                )),
                                            ]
                                        ),

                                        # Both scenarios' projections on one chart
                                        html.Div(
                                            id="graph-container-projection-tab3",
                                            style=graph_card_style,
                                            children=[dcc.Loading(id="graph-loading-tab3", type="default", color="#3B82F6",
                                                children=dcc.Graph(id="progress-graph-projection-tab3", style={"minHeight": 440}))]
                                        ),

                                    ], style={"flex": "5 1 0", "minWidth": "300px", "paddingLeft": "16px"}),

                                ], style={"display": "flex", "flexDirection": "row", "gap": "16px", "alignItems": "flex-start", "padding": "12px"}),
                            ]
                        ),
                    ]
                )
            ], style=card_style)
        ])])  # closes main content Div + root Div

    return layout
