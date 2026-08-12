from dash import html, dcc, dash_table
from config import app_color
import dash_bootstrap_components as dbc
import time
import inspect

# === Module-level style defaults (overridden by locals inside create_layout) ===
label_style = {"fontWeight": 600, "display": "block", "marginBottom": 4}
dropdown_style = {"width": "100%"}
input_style = {"width": "100%"}


def create_layout(pi_training_options, team_options, pi_display_options):

    card_style = {
        "backgroundColor": "#FFFFFF",
        "padding": "20px",
        "borderRadius": "12px",
        "marginBottom": "25px",
        "boxShadow": "0 2px 10px rgba(0,0,0,0.08)"
    }

    label_style = {
        "color": app_color["text"],
        "fontWeight": "bold",
        "marginBottom": "6px"
    }

    input_style = {
        "width": "100%",
        "backgroundColor": "#E5E9F0",
        "color": "#1F2937",
        "border": "1px solid white",
        "padding": "0 10px",
        "fontSize": "14px",
        "borderRadius": "6px",
        "height": "42px",
        "lineHeight": "42px"
    }

    dropdown_style = {
        "width": "100%",
        "minWidth": "250px",
        "backgroundColor": "#E5E9F0",
        "color": "white",
        "fontSize": "14px",
        "border": "1px solid white",
        "borderRadius": "6px",
        "height": "42px",
        "lineHeight": "42px"
    }

    button_style = {
        "backgroundColor": app_color["accent"],
        "color": "white",
        "border": "none",
        "padding": "10px 20px",
        "fontWeight": "bold",
        "borderRadius": "6px",
        "cursor": "pointer",
        "transition": "background-color 0.3s"
    }

    button_style_alt = {
        "backgroundColor": "#1F2937",
        "color": "white",
        "border": "none",
        "padding": "10px 20px",
        "fontWeight": "bold",
        "borderRadius": "6px",
        "cursor": "pointer",
        "transition": "background-color 0.3s"
    }

    header_style = {
        "textAlign": "left",
        "color": "#1F2937",
        "border": "white",
        "borderBottom": "2px solid #000000",
        "paddingBottom": "5px"
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
        "backgroundColor": "transparent",
        "color": "#6B7280",
        "border": "none",
        "borderRadius": "8px",
        "padding": "10px 16px",
        "cursor": "pointer",
        "textAlign": "left",
    }

    datepicker_style = {
        "display": "block",
        "width": "100%",
        "minWidth": "250px",
        "fontSize": "14px",
    }

    dcc.Store(id="training_df"),
    dcc.Store(id="training_items"),

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

                                    # LEFT SIDE
                                    html.Div([

                                        html.Div([
                                            html.Div([
                                                html.Label("Select Team", style=label_style),
                                                dcc.Dropdown(
                                                    id="team-dropdown",
                                                    options=[{"label": t, "value": t} for t in team_options],
                                                    value=team_options[-2] if team_options else None,
                                                    multi=False,
                                                    style=dropdown_style
                                                )
                                            ], style={"width": "70%", "padding": "0 10px", "minWidth": "250px"})
                                        ], style={"display": "flex", "flexWrap": "wrap", "gap": "15px", "marginBottom": "10px"}),

                                        html.Div([
                                            html.Div([
                                                html.Label("Select PI increment to analyze", style=label_style),
                                                dcc.Dropdown(
                                                    id="pi-display-dropdown",
                                                    options=pi_display_options,
                                                    value="",
                                                    multi=False,
                                                    style=dropdown_style
                                                )
                                            ], style={"width": "70%", "padding": "0 10px"}),

                                            html.Div([
                                                html.Label("PI Start Date", style=label_style),
                                                dcc.DatePickerSingle(
                                                    id="sprint-startdate-input",
                                                    date="2024-01-05",
                                                    display_format="YYYY-MM-DD",
                                                    placeholder="Select a date",
                                                    style=datepicker_style
                                                )
                                            ], style={"width": "70%", "padding": "0 10px"}),

                                            html.Div([
                                                html.Label("Committed PI Completion Date", style=label_style),
                                                dcc.DatePickerSingle(
                                                    id="estimated-project-completion-date",
                                                    date="2025-05-28",
                                                    display_format="YYYY-MM-DD",
                                                    placeholder="Select a date",
                                                    style=datepicker_style
                                                )
                                            ], style={"width": "70%", "padding": "0 10px"}),

                                            html.Div([
                                                html.Label("Projection Start Date", style=label_style),
                                                dcc.DatePickerSingle(
                                                    id="todays-date",
                                                    date="2026-04-01",
                                                    display_format="YYYY-MM-DD",
                                                    placeholder="Select a date",
                                                    style=datepicker_style
                                                ),
                                                html.Div(id="todays-date-output")
                                            ], style={"width": "70%", "padding": "0 10px"}),

                                        ], style={"display": "flex", "flexWrap": "wrap", "gap": "15px", "marginBottom": "10px"}),

                                        html.Div([
                                            dbc.Button(
                                                "Advanced options",
                                                id="advanced-button",
                                                className="mb-2",
                                                color="secondary",
                                                outline=True,
                                                n_clicks=0,
                                            ),
                                            dbc.Collapse(
                                                id="advanced-collapse",
                                                is_open=False,
                                                children=dbc.Card(
                                                    dbc.CardBody([

                                                        html.Div([
                                                            html.Label("Choose issues to examine All/Just Stories", style=label_style),
                                                            dcc.Dropdown(
                                                                id="issue_type_selection",
                                                                options=[{"label": x, "value": x} for x in ["Just Stories", "All Issues"]],
                                                                value="Just Stories",
                                                                multi=False,
                                                                style=dropdown_style
                                                            )
                                                        ], style={"padding": "0 10px"}),

                                                        html.Div([
                                                            html.Label("Team Size", style=label_style),
                                                            dcc.Input(
                                                                id="team_size",
                                                                type="number",
                                                                value=6,
                                                                placeholder="e.g. 15",
                                                                style=input_style
                                                            )
                                                        ], style={"flex": 1, "padding": "0 10px"}),

                                                        html.Div([
                                                            html.Label("Select PI(s) to train on", style=label_style),
                                                            dcc.Dropdown(
                                                                id="pi-training-dropdown",
                                                                options=pi_training_options,
                                                                value="",
                                                                multi=True,
                                                                style=dropdown_style
                                                            )
                                                        ], style={"flex": "1", "padding": "0 10px", "marginBottom": "50px"})
                                                    ])
                                                )
                                            )
                                        ], style={"paddingLeft": "10px"}),

                                        html.Div([
                                            html.Button(
                                                "Get Graph",
                                                id="generate-graph-projections-btn",
                                                n_clicks=0,
                                                style=button_style
                                            )
                                        ], style={"marginBottom": "15px"}),

                                        dcc.Store(id="stored-projection-data"),
                                        dcc.Store(id='pi-data-all-issues')

                                    ], style={
                                        "flex": "2 1 0",
                                        "minWidth": "300px",
                                        "paddingLeft": "16px",
                                        "display": "flex",
                                        "flexDirection": "column",
                                        "gap": "12px"
                                    }),

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

                                            html.Div(
                                                style={
                                                    "display": "flex",
                                                    "gap": "12px",
                                                    "minHeight": "100px",
                                                },
                                                children=[
                                                    html.Div(
                                                        style={
                                                            "flex": "1",
                                                            "display": "grid",
                                                            "gridTemplateColumns": "1fr 1fr 1fr",
                                                            "gridTemplateRows": "1fr",
                                                            "gap": "8px",
                                                        },
                                                        children=[
                                                            html.Div(
                                                                id="mini-card-1",
                                                                style={
                                                                    "backgroundColor": "#FFFFFF",
                                                                    "borderRadius": "8px",
                                                                    "padding": "12px",
                                                                    "boxShadow": "0 2px 6px rgba(0,0,0,0.1)",
                                                                    "textAlign": "center",
                                                                },
                                                                children=[
                                                                    html.H5("Probability of finishing by PI end date"),
                                                                    html.H1(id="probability_finishing_by_completion_date")
                                                                ]
                                                            ),
                                                            html.Div(
                                                                id="mini-card-2",
                                                                style={
                                                                    "backgroundColor": "#FFFFFF",
                                                                    "borderRadius": "8px",
                                                                    "padding": "12px",
                                                                    "boxShadow": "0 2px 6px rgba(0,0,0,0.1)",
                                                                    "textAlign": "center",
                                                                },
                                                                children=[
                                                                    html.H5("Projected date of completion"),
                                                                    html.H3(id="completion-date"),
                                                                ]
                                                            ),
                                                            html.Div(
                                                                id="mini-card-3",
                                                                style={
                                                                    "backgroundColor": "#FFFFFF",
                                                                    "borderRadius": "8px",
                                                                    "padding": "12px",
                                                                    "boxShadow": "0 2px 6px rgba(0,0,0,0.1)",
                                                                    "textAlign": "center",
                                                                },
                                                                children=[
                                                                    html.H5("Projected percent complete at PI end date"),
                                                                    html.H1(id="projected-projection-completion"),
                                                                ]
                                                            ),
                                                        ]
                                                    ),
                                                ]
                                            ),

                                            html.Div(
                                                id="graph-container-projection",
                                                style={
                                                    "backgroundColor": "#FFFFFF",
                                                    "borderRadius": "12px",
                                                    "padding": "12px",
                                                    "boxShadow": "0 2px 10px rgba(0,0,0,0.1)",
                                                },
                                                children=[
                                                    dcc.Loading(
                                                        id="graph-loading",
                                                        type="default",
                                                        color="#3B82F6",
                                                        children=dcc.Graph(id="progress-graph-projection", style={"minHeight": 380})
                                                    )
                                                ]
                                            ),

                                            html.Div(
                                                style={
                                                    "flex": "1",
                                                    "display": "grid",
                                                    "gridTemplateColumns": "1fr 1fr",
                                                    "gridTemplateRows": "1fr",
                                                    "gap": "8px",
                                                },
                                                children=[
                                                    html.Div(
                                                        id = "incomplete_issue_display",
                                                        style = {
                                                            "backgroundColor": "#FFFFFF",
                                                            "borderRadius": "8px",
                                                            "padding": "12px",
                                                            "boxShadow": "0 2px 6px rgba(0,0,0,0.1)",
                                                            "textAlign": "left",
                                                        },
                                                        children=[
                                                            html.H5("Status of remaining issues"),
                                                            html.P(id="remaining_issue_display"),
                                                            html.H5("Deviance of issues actually completed vs in sprint log")
                                                        ]
                                                    ),
                                                    html.Div(
                                                        id = "sprint_plan_display",
                                                        style = {
                                                            "backgroundColor": "#FFFFFF",
                                                            "borderRadius": "8px",
                                                            "padding": "12px",
                                                            "boxShadow": "0 2px 6px rgba(0,0,0,0.1)",
                                                            "textAlign": "left",
                                                        },
                                                        children=[
                                                            html.H5("Sprint Plan"),
                                                            html.Pre(id="sprint_plan_display_string", 
                                                                     style={"margin": 0, "fontFamily": "inherit", "fontSize": "inherit"})
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

                                html.Div(
                                    style={
                                        "display": "flex",
                                        "flexDirection": "row",
                                        "gap": "16px",
                                        "alignItems": "flex-start",
                                    },
                                    children=[

                                        # LEFT SIDE
                                        html.Div(
                                            style={
                                                "flex": "2 1 0",
                                                "minWidth": "300px",
                                                "paddingLeft": "16px",
                                                "display": "flex",
                                                "flexDirection": "column",
                                                "gap": "8px",
                                            },
                                            children=[
                                                html.H3("Rules tracked:", style={"margin": "0", "color": "#1F2937"}),
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
                                                        html.Button("WIP",            id={"type": "v-option", "index": 0}, style=selected_style),
                                                        html.Button("Sprint Plan Deviance",   id={"type": "v-option", "index": 1}, style=unselected_style),
                                                    ]
                                                ),
                                            ]
                                        ),

                                        # RIGHT SIDE

                                        dcc.Store(id = "wip-figure-store"), 
                                        dcc.Store(id = "sprint-figure-store"), 

                                        html.Div(
                                            style={
                                                'display': 'flex',
                                                'flexDirection': 'column',
                                                'gap': '20px',
                                                'flex': '5 1 0',
                                                'minWidth': '300px',
                                                'paddingLeft': '16px',
                                            },
                                            children=[

                                                html.Div(
                                                    style={
                                                        "display": "flex",
                                                        "gap": "12px",
                                                        "minHeight": "100px",
                                                    },
                                                    children=[
                                                        html.Div(
                                                            id="top-left-card-tab2",
                                                            style={
                                                                "flex": "2",
                                                                "backgroundColor": "rgba(255, 0, 0, 0.15)",
                                                                "borderRadius": "12px",
                                                                "padding": "16px",
                                                                "boxShadow": "0 2px 10px rgba(0,0,0,0.1)",
                                                                "border": "1px solid #FF0000",
                                                            },
                                                            children=[
                                                                html.H3("WIP Delay:"),
                                                                html.P("Threshold of work in progress items exceeded. This will delay project completion unless resolved.")
                                                            ]
                                                        ),
                                                    ]
                                                ),

                                                html.Div(
                                                    style={"flex": "4"},
                                                    children=[
                                                        html.Div(
                                                            id="graph-container-projection-tab2",
                                                            style={
                                                                "backgroundColor": "#FFFFFF",
                                                                "borderRadius": "12px",
                                                                "padding": "12px",
                                                                "boxShadow": "0 2px 10px rgba(0,0,0,0.1)",
                                                            },
                                                            children=[
                                                                dcc.Loading(
                                                                    id="graph-loading-tab2",
                                                                    type="default",
                                                                    color="#3B82F6",
                                                                    children=dcc.Graph(id="progress-graph-projection-tab2", style={"minHeight": 380})
                                                                )
                                                            ]
                                                        )
                                                    ]
                                                )

                                            ]
                                        )

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

                                    # LEFT SIDE — only team size
                                    html.Div([
                                        html.Div([
                                            html.Div([
                                                html.Label("Team Size", style=label_style),
                                                dcc.Input(
                                                    id="team_size-tab3",
                                                    type="number",
                                                    value=6,
                                                    placeholder="e.g. 15",
                                                    style=input_style
                                                )
                                            ], style={"flex": 1, "padding": "0 10px"}),

                                        ], style={"display": "flex", "flexWrap": "wrap", "gap": "15px", "marginBottom": "10px"}),

                                        html.Div([
                                            html.Button(
                                                "Get Graph",
                                                id="generate-graph-projections-btn-tab3",
                                                n_clicks=0,
                                                style=button_style
                                            )
                                        ], style={"marginBottom": "15px"}),

                                    ], style={
                                        "flex": "2 1 0",
                                        "minWidth": "300px",
                                        "paddingLeft": "16px",
                                        "display": "flex",
                                        "flexDirection": "column",
                                        "gap": "12px"
                                    }),

                                    # RIGHT SIDE
                                    html.Div([
                                        html.H4("Comparison Table: Scenario 1 vs Scenario 2 Metrics"),
                                        html.Div(
                                            style={
                                                "flex": "1",
                                                "display": "grid",
                                                "gridTemplateColumns": "1fr 1fr 1fr",
                                                "gridTemplateRows": "1fr 1fr",
                                                "gap": "8px",
                                            },
                                            children=[
                                                html.Div(id="mini-card-1-team1-tab3", style={"backgroundColor": "#FFFFFF", "borderRadius": "8px", "padding": "12px", "boxShadow": "0 2px 6px rgba(0,0,0,0.1)", "textAlign": "center"},
                                                    children=[html.P("Probability of finishing by PI end date"), html.H1(id="probability_finishing_by_completion_date-team1-tab3")]),
                                                html.Div(id="mini-card-2-team1-tab3", style={"backgroundColor": "#FFFFFF", "borderRadius": "8px", "padding": "12px", "boxShadow": "0 2px 6px rgba(0,0,0,0.1)", "textAlign": "center"},
                                                    children=[html.P("Projected date of completion"), html.H3(id="completion-date-team1-tab3")]),
                                                html.Div(id="mini-card-3-team1-tab3", style={"backgroundColor": "#FFFFFF", "borderRadius": "8px", "padding": "12px", "boxShadow": "0 2px 6px rgba(0,0,0,0.1)", "textAlign": "center"},
                                                    children=[html.P("Projected percent complete at PI end date"), html.H1(id="projected-projection-completion-team1-tab3")]),
                                                html.Div(id="mini-card-1-team2-tab3", style={"backgroundColor": "#FFFFFF", "borderRadius": "8px", "padding": "12px", "boxShadow": "0 2px 6px rgba(0,0,0,0.1)", "textAlign": "center"},
                                                    children=[html.P("Probability of finishing by PI end date"), html.H1(id="probability_finishing_by_completion_date-team2-tab3")]),
                                                html.Div(id="mini-card-2-team2-tab3", style={"backgroundColor": "#FFFFFF", "borderRadius": "8px", "padding": "12px", "boxShadow": "0 2px 6px rgba(0,0,0,0.1)", "textAlign": "center"},
                                                    children=[html.P("Projected date of completion"), html.H3(id="completion-date-team2-tab3")]),
                                                html.Div(id="mini-card-3-team2-tab3", style={"backgroundColor": "#FFFFFF", "borderRadius": "8px", "padding": "12px", "boxShadow": "0 2px 6px rgba(0,0,0,0.1)", "textAlign": "center"},
                                                    children=[html.P("Projected percent complete at PI end date"), html.H1(id="projected-projection-completion-team2-tab3")]),
                                            ]
                                        ),

                                        html.H4("Scenario 1"),
                                        html.Div(
                                            id="graph-container-projection-tab3-team1",
                                            style={"backgroundColor": "#FFFFFF", "borderRadius": "12px", "padding": "12px", "boxShadow": "0 2px 10px rgba(0,0,0,0.1)", "marginBottom": "10px"},
                                            children=[dcc.Loading(id="graph-loading-tab3-team1", type="default", color="#3B82F6",
                                            children=dcc.Graph(id="progress-graph-projection-tab3-team1", style={"minHeight": 380}))]
                                        ),

                                        html.H4("Scenario 2"),
                                        html.Div(
                                            id="graph-container-projection-tab3-team2",
                                            style={"backgroundColor": "#FFFFFF", "borderRadius": "12px", "padding": "12px", "boxShadow": "0 2px 10px rgba(0,0,0,0.1)"},
                                            children=[dcc.Loading(id="graph-loading-tab3-team2", type="default", color="#3B82F6",
                                                children=dcc.Graph(id="progress-graph-projection-tab3-team2", style={"minHeight": 380}))]
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
