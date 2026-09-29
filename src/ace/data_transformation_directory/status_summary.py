# ---------------------------------------------------------------------------
# status_summary.py
#
# Builds the data behind the "Status of remaining issues" card:
#
#   1. Where every issue stood on the projection start date
#      (done / in progress / not started / parked), with the open issues listed.
#   2. Why the Actual In Progress line tops out below 100%: which story points
#      have never entered "In Progress", broken down by state.
#   3. How started and finished work compare with the baseline plan on that
#      date, in story points (the plan and the actuals use different totals,
#      so percentages alone would mislead).
#
# Everything is computed from transitions as they stood on the as-of date,
# so the card matches the chart at the "Today" line.
# ---------------------------------------------------------------------------

import datetime
from datetime import timedelta

import polars as pl

from data_transformation_directory.Actuals import _get_done_date, _get_start_date
from data_transformation_directory.Db import _normalize_to_naive

# States that mean "not being worked, not expected soon". Kept in step with
# EXCLUDED_STATES in pipeline.py (minus Done / Realized, which count as done).
PARKED_STATES = {
    "Backlog", "Inactive", "On Hold", "Blocked",
    "Triage", "Triaging", "Awaiting Approval",
}


def _transitions_as_of(item, cutoff: datetime.datetime):
    t = item.transitions
    if t is None or t.is_empty() or "start" not in t.columns:
        return t
    return t.filter(pl.col("start") < cutoff)


def _state_as_of(item, t) -> tuple[str, datetime.datetime | None]:
    """Jira state on the as-of date, and when the issue entered it."""
    if item.transitions is None or item.transitions.is_empty():
        return item.current_state or "To Do", None
    if t is None or t.is_empty():
        return "To Do", None                   # hadn't moved yet
    last = t.sort("start").tail(1)
    return last["jira_state"][0], _normalize_to_naive(last["start"][0])


def build_status_summary(
    all_issues:          list,
    all_issues_baseline: list,
    combined_df:         pl.DataFrame,
    as_of_date:          str,
    planned_total_sp:    float | None = None,
) -> dict:
    """
    Returns a JSON-serialisable dict for utils.status_view.render_status_summary.
    """
    as_of  = datetime.date.fromisoformat(str(as_of_date)[:10])
    cutoff = datetime.datetime.combine(as_of + timedelta(days=1), datetime.time())  # end of as-of day

    buckets = {k: {"sp": 0.0, "count": 0} for k in ("done", "in_progress", "not_started", "parked")}
    done_without_start = {"sp": 0.0, "count": 0}
    unstarted_by_state: dict[str, dict] = {}
    open_issues: list[dict] = []
    started_sp = 0.0
    total_sp   = 0.0

    for item in all_issues:
        sp        = float(item.weight) if item.weight else 0.0
        total_sp += sp

        t        = _transitions_as_of(item, cutoff)
        done_dt  = _get_done_date(t)
        start_dt = _get_start_date(t)
        state, since = _state_as_of(item, t)

        if start_dt is not None:
            started_sp += sp

        if done_dt is not None:
            bucket = "done"
            if start_dt is None:
                done_without_start["sp"]    += sp
                done_without_start["count"] += 1
        elif start_dt is not None:
            bucket = "in_progress"
        elif state in PARKED_STATES:
            bucket = "parked"
        else:
            bucket = "not_started"

        buckets[bucket]["sp"]    += sp
        buckets[bucket]["count"] += 1

        if bucket in ("not_started", "parked"):
            entry = unstarted_by_state.setdefault(state, {"state": state, "sp": 0.0, "count": 0, "parked": bucket == "parked"})
            entry["sp"]    += sp
            entry["count"] += 1

        if bucket != "done":
            open_issues.append({
                "key":           item.key,
                "state":         state,
                "sp":            round(sp, 2),
                "started":       bucket == "in_progress",
                "days_in_state": (cutoff - since).days if since else None,
            })

    # In progress first, then longest-stuck first
    open_issues.sort(key=lambda r: (not r["started"], -(r["days_in_state"] or 0)))

    # ── Plan on the as-of date ────────────────────────────────────────────────
    to_date = combined_df.filter(pl.col("date") < cutoff).sort("date")
    expected_sp = float(to_date["baseline_sp"][-1]) if to_date.height else 0.0
    if planned_total_sp is None:
        planned_total_sp = float(combined_df["baseline_sp"].max())

    # ── Scope: which issues are in the plan vs the actual lines ───────────────
    baseline_map = {i.key: i for i in all_issues_baseline}
    actual_map   = {i.key: i for i in all_issues}
    baseline_only = [i for k, i in baseline_map.items() if k not in actual_map]
    actual_only   = [i for k, i in actual_map.items()   if k not in baseline_map]
    sp_changed    = [
        k for k in baseline_map.keys() & actual_map.keys()
        if (baseline_map[k].weight or 0) != (actual_map[k].weight or 0)
    ]

    def _sp(items):
        return round(sum(float(i.weight or 0) for i in items), 2)

    def _round(d):
        return {k: round(v, 2) if isinstance(v, float) else v for k, v in d.items()}

    return {
        "as_of":              as_of.isoformat(),
        "total_sp":           round(total_sp, 2),
        "total_issues":       len(all_issues),
        "buckets":            {k: _round(v) for k, v in buckets.items()},
        "started_sp":         round(started_sp, 2),
        "done_without_start": _round(done_without_start),
        "unstarted_by_state": sorted((_round(v) for v in unstarted_by_state.values()), key=lambda r: -r["sp"]),
        "open_issues":        open_issues,
        "plan": {
            "expected_sp":   round(expected_sp, 2),
            "planned_total": round(planned_total_sp, 2),
        },
        "scope": {
            "baseline_only_sp":    _sp(baseline_only),
            "baseline_only_count": len(baseline_only),
            "actual_only_sp":      _sp(actual_only),
            "actual_only_count":   len(actual_only),
            "sp_changed_count":    len(sp_changed),
        },
    }
