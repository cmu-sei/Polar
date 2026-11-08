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

"""House the base item for work."""
from .jira_map import STATE_LIST


class BaseItem:
    """This will be used for a common item to project."""

    def __init__(self):
        """Initialize the class."""
        self.durations = {}
        self.links = []
        self.children = []
        self.key = ''
        self.type = ''
        self.create = ''
        self.sprint = ''
        self.increment = ''
        self.estimate = ''
        self.team = ''
        self.teams = []
        self.current_state = ''
        self.resolution = ''
        self.weight = 0
        self.adjusted_weight = 0
        self.orig_adj_weight = 0
        self.transitions = []
        self.sprints = []
        self.parent = ''
        self.parent_keys = []
        self.raw_data = {}
        self.is_late = False
        self.in_prog_date = ''
        self.done_date = ''
        self.last_transition = ''

        for field in STATE_LIST:
            self.durations[f'{field} Start'] = None
            self.durations[f'{field} End'] = None
            self.durations[f'{field} Active Sec Accrual'] = float(0)
            self.durations[f'{field} Active Dec Accrual'] = float(0)
            self.durations[f'{field} Active Count'] = 0
            self.durations[f'{field} Idle Count'] = 0
            self.durations[f'{field} Idle Sec Accrual'] = float(0)
            self.durations[f'{field} Idle Dec Accrual'] = float(0)
            self.durations[f'Total {field}'] = 0
            self.durations[f'Total {field} Dec'] = float(0)
    
    def __repr__(self):
        # Helper: show attribute if present, else "?"
        def g(name, default="?"):
            return getattr(self, name, default)

        # Some codebases store state under different names.
        # Try a small set of likely candidates.
        state = g("state", None)
        if state in (None, "?"):
            state = g("status", None)
        if state in (None, "?"):
            state = g("workflow_state", None)

        # Raw data fallback (if you keep the underlying payload)
        raw = g("raw_data", None)
        if state in (None, "?") and isinstance(raw, dict):
            # common shapes: {"state": ...} or {"fields": {"status": {"name": ...}}}
            if "state" in raw:
                state = raw.get("state")
            elif "fields" in raw and isinstance(raw["fields"], dict):
                status = raw["fields"].get("status")
                if isinstance(status, dict) and "name" in status:
                    state = status["name"]

        # Keep it short but informative
        key = g("key", g("id", g("issue_key", "?")))
        typ = g("type", g("issue_type", "?"))

        # weights vary by your code; try a couple
        sp = g("adjusted_weight", g("sp", g("story_points", "?")))

        # durations can be huge; show just whether present
        durations = g("durations", None)
        dur_note = "yes" if isinstance(durations, dict) and durations else "no"

        return f"BaseItem(key={key!r}, type={typ!r}, state={state!r}, sp={sp!r}, durations={dur_note})"

    __str__ = __repr__
