# ---------------------------------------------------------------------------
# increments.py
#
# Context about where the selected PI sits relative to the one before it:
#
#   - the prior increment's dates (from the team's sprint names), so charts
#     can mark where it ends and the current one begins
#   - issues carried over from earlier increments: issues in this PI's work
#     that sat in an earlier PI's sprint and weren't finished before this
#     PI started
#
# The result is attached to sprint_plan["increments"] in pipeline.py, so it
# travels to the dashboard with the rest of the plan.
# ---------------------------------------------------------------------------

import re
from datetime import date

from data_transformation_directory.Actuals import _get_done_date
from data_transformation_directory.Baseline import _clean_sprint_name, _sprint_belongs_to_team
from data_transformation_directory.Db import _normalize_to_naive

_PI_RE = re.compile(r"\bPI-(\d+)\b", re.IGNORECASE)


def _pi_number(text: str | None) -> int | None:
    m = _PI_RE.search(text or "")
    return int(m.group(1)) if m else None


def _team_sprints(item, team_name: str | None):
    """Yield (name, start, end) for the item's sprints that belong to the team."""
    for s in item.sprints or []:
        name  = _clean_sprint_name(s.get("name", ""))
        start = _normalize_to_naive(s.get("startDate"))
        end   = _normalize_to_naive(s.get("endDate"))
        if not name or start is None or end is None:
            continue
        if team_name and not _sprint_belongs_to_team(name, team_name):
            continue
        yield name, start, end


def _short_name(name: str, team_name: str | None) -> str:
    if team_name and name.lower().startswith(team_name.lower()):
        return name[len(team_name):].strip(" -_") or name
    return name


def build_increment_context(
    all_issues:          list,
    all_issues_baseline: list,
    team_name:           str | None,
    increment:           str | None,
    sprint_plan:         dict,
) -> dict:
    """
    Returns a JSON-serialisable dict:
        current      {label, start, end}
        prior        {label, start, end} or None
        overlap_days days the prior PI's last sprint runs past this PI's start (0 if none)
        carryover    list of {key, sp, from_increment, last_prior_sprint,
                              replanned, state, done_date}
    """
    current_num = _pi_number(increment)
    context = {
        "current":      {"label": increment, "start": sprint_plan["pi_start"], "end": sprint_plan["pi_end"]},
        "prior":        None,
        "overlap_days": 0,
        "carryover":    [],
    }
    if current_num is None:
        return context

    current_start = date.fromisoformat(sprint_plan["pi_start"])
    prefix = _PI_RE.search(increment).group(0).rsplit("-", 1)[0]   # keeps "PI" as written

    # ── Prior increment dates ─────────────────────────────────────────────────
    sprint_dates: dict[str, tuple] = {}
    for item in list(all_issues) + list(all_issues_baseline):
        for name, start, end in _team_sprints(item, team_name):
            sprint_dates.setdefault(name, (start, end))

    earlier = [n for n in (_pi_number(name) for name in sprint_dates) if n is not None and n < current_num]
    if earlier:
        prior_num = max(earlier)
        prior_sprints = [(s, e) for name, (s, e) in sprint_dates.items() if _pi_number(name) == prior_num]
        prior_start = min(s for s, _ in prior_sprints).date()
        prior_end   = max(e for _, e in prior_sprints).date()
        context["prior"] = {
            "label": f"{prefix}-{prior_num}",
            "start": prior_start.isoformat(),
            "end":   prior_end.isoformat(),
        }
        context["overlap_days"] = max(0, (prior_end - current_start).days)

    # ── Carried-over issues ───────────────────────────────────────────────────
    for item in all_issues:
        earlier_sprints = []
        in_current      = False
        for name, _, end in _team_sprints(item, team_name):
            n = _pi_number(name)
            if n is None:
                continue
            if n < current_num:
                earlier_sprints.append((end, name, n))
            elif n == current_num:
                in_current = True

        if not earlier_sprints:
            continue

        done_dt = _get_done_date(item.transitions)
        if done_dt is not None and done_dt.date() < current_start:
            continue                                    # finished before this PI began

        _, last_name, last_num = max(earlier_sprints)
        context["carryover"].append({
            "key":               item.key,
            "sp":                round(float(item.weight or 0), 2),
            "from_increment":    f"{prefix}-{last_num}",
            "last_prior_sprint": _short_name(last_name, team_name),
            "replanned":         in_current,
            "state":             item.current_state,
            "done_date":         done_dt.date().isoformat() if done_dt else None,
        })

    # Open issues first, then largest
    context["carryover"].sort(key=lambda r: (r["done_date"] is not None, -r["sp"]))
    return context
