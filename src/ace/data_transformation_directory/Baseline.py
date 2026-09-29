# ---------------------------------------------------------------------------
# baseline.py
#
# Builds the piecewise-linear planned baseline for a PI.
# Each sprint gets its own slope proportional to the SP assigned to it,
# producing a staircase-of-ramps rather than a single diagonal.
#
# Only sprints that belong to the selected team AND program increment count
# toward the baseline. Issues whose sprints are all in other PIs (carry-over
# work that was never re-planned into this PI's sprints) are reported
# separately and excluded from the baseline. They still appear in actuals,
# because actuals are built from a separate issue list.
# ---------------------------------------------------------------------------

import datetime
import re
from datetime import timedelta

import polars as pl

from data_transformation_directory.Db import _normalize_to_naive


# ---------------------------------------------------------------------------
# Sprint name helpers
# ---------------------------------------------------------------------------

def _clean_sprint_name(name: str) -> str:
    """Strip the rapidViewId suffix Jira appends to sprint names."""
    return name.split(",rapidViewId=")[0].strip() if name else name


def _sprint_belongs_to_team(sprint_name: str, team_name: str) -> bool:
    """
    True if the sprint name starts with the team name (case-insensitive).
    "Thestrals PI-29 J" matches "Thestrals"; "Keymasters 2025 J" does not.
    """
    return sprint_name.lower().startswith(team_name.lower())


def _sprint_in_increment(sprint_name: str, increment: str) -> bool:
    """
    True if the sprint name contains the increment label as a whole token.
    "Thestrals PI-33 M (IP)" matches "PI-33" but not "PI-3" or "PI-330".
    """
    if not sprint_name or not increment:
        return False
    pattern = rf"\b{re.escape(increment.strip())}\b"
    return re.search(pattern, sprint_name, re.IGNORECASE) is not None


def _sprint_belongs_to_team_and_pi(
    sprint_name: str, team_name: str | None, increment: str | None
) -> bool:
    """True if the sprint belongs to the team and the PI. None skips that check."""
    if team_name and not _sprint_belongs_to_team(sprint_name, team_name):
        return False
    if increment and not _sprint_in_increment(sprint_name, increment):
        return False
    return True


def _short_sprint_label(name: str, team_name: str | None, increment: str | None) -> str:
    """'Thestrals PI-33 M (IP)' -> 'M (IP)'. Falls back to the full name."""
    label = name
    if team_name and label.lower().startswith(team_name.lower()):
        label = label[len(team_name):]
    if increment:
        label = re.sub(rf"\b{re.escape(increment.strip())}\b", "", label, flags=re.IGNORECASE)
    label = label.strip(" -_")
    return label or name


def _is_ip_sprint(name: str) -> bool:
    return re.search(r"\bIP\b", name) is not None


# ---------------------------------------------------------------------------
# Fallback: date-window detection (used only when no increment is given or
# no sprint names contain the increment label)
# ---------------------------------------------------------------------------

def _get_pi_date_window(all_issues: list) -> tuple[datetime.datetime, datetime.datetime] | None:
    all_ends: list[datetime.datetime] = []
    for item in all_issues:
        for s in item.sprints:
            end = _normalize_to_naive(s.get("endDate"))
            if end is not None:
                all_ends.append(end)

    if len(all_ends) < 2:
        return None

    all_ends.sort()

    GAP_THRESHOLD  = timedelta(days=21)
    best_start_idx = 0
    best_len       = 1
    cur_start_idx  = 0

    for i in range(1, len(all_ends)):
        if (all_ends[i] - all_ends[i - 1]) > GAP_THRESHOLD:
            if (i - cur_start_idx) > best_len:
                best_len       = i - cur_start_idx
                best_start_idx = cur_start_idx
            cur_start_idx = i

    if (len(all_ends) - cur_start_idx) > best_len:
        best_start_idx = cur_start_idx

    cluster_end   = all_ends[-1] if cur_start_idx == best_start_idx else all_ends[best_start_idx + best_len - 1]
    cluster_start = all_ends[best_start_idx]

    return cluster_start - timedelta(days=21), cluster_end + timedelta(days=1)


# ---------------------------------------------------------------------------
# Sprint selection
# ---------------------------------------------------------------------------

def _get_target_sprint_name(
    sprints:   list[dict],
    pi_start:  datetime.datetime | None,
    pi_end:    datetime.datetime | None,
    team_name: str | None = None,
    increment: str | None = None,
) -> str | None:
    """
    Return the LAST sprint (by end date) the issue is assigned to that belongs
    to the team and increment. The date window is applied only if provided.
    """
    candidates = []
    for s in sprints:
        end  = _normalize_to_naive(s.get("endDate"))
        name = _clean_sprint_name(s.get("name", ""))
        if end is None:
            continue
        if pi_start is not None and not (pi_start <= end <= pi_end):
            continue
        if not _sprint_belongs_to_team_and_pi(name, team_name, increment):
            continue
        candidates.append((end, name))

    return max(candidates, key=lambda x: x[0])[1] if candidates else None


def _collect_sprints(all_issues, pi_start, pi_end, team_name, increment) -> dict[str, dict]:
    meta: dict[str, dict] = {}
    for item in all_issues:
        for s in item.sprints:
            name  = _clean_sprint_name(s.get("name", ""))
            start = _normalize_to_naive(s.get("startDate"))
            end   = _normalize_to_naive(s.get("endDate"))
            if not (name and start is not None and end is not None):
                continue
            if pi_start is not None and not (pi_start <= end <= pi_end):
                continue
            if not _sprint_belongs_to_team_and_pi(name, team_name, increment):
                continue
            meta.setdefault(name, {"sprint_start": start, "sprint_end": end})
    return meta


# ---------------------------------------------------------------------------
# Plain-text rendering of the plan (for logs)
# ---------------------------------------------------------------------------

def format_sprint_plan_text(plan: dict) -> str:
    lines = [f"{len(plan['sprints'])} sprints | Total SP: {plan['total_sp']:.1f}"]
    for s in plan["sprints"]:
        lines.append(
            f"  {s['name']}  {s['start']} → {s['end']}  |  {s['sp']:.1f} SP "
            f"({s['pct']:.1f}%)  |  {s['sp_per_week']:.1f} SP/week"
        )
    if plan["carryover_sp"] > 0:
        lines.append(
            f"Other PIs: {plan['carryover_sp']:.1f} SP ({plan['carryover_count']} issues) "
            "— excluded from baseline"
        )
    if plan["unassigned_sp"] > 0:
        lines.append(
            f"No sprint: {plan['unassigned_sp']:.1f} SP ({plan['unassigned_count']} issues) "
            "— excluded from baseline"
        )
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Baseline construction
# ---------------------------------------------------------------------------

def build_pi_baseline(
    all_issues: list,
    team_name:  str | None = None,
    increment:  str | None = None,
):
    """
    Build a piecewise-linear baseline (0% → 100%) across the PI.

    Parameters
    ----------
    all_issues : list[BaseItem] with .key, .weight and .sprints
    team_name  : team prefix used in sprint names, e.g. "Thestrals"
    increment  : PI label used in sprint names, e.g. "PI-33". When given,
                 only sprints whose names contain it count toward the baseline.

    Returns
    -------
    (baseline_df, sprint_plan, spike_dates)
        baseline_df : columns date, sprint_name, baseline_sp, baseline_pct
        sprint_plan : dict describing the plan (see bottom of function);
                      JSON-serialisable so it can go in a dcc.Store
        spike_dates : sprint start dates where planned SP jumped > 50%
    """

    # ── 1. Build sprint table ─────────────────────────────────────────────────
    pi_start = pi_end = None
    sprint_meta: dict[str, dict] = {}

    if increment:
        sprint_meta = _collect_sprints(all_issues, None, None, team_name, increment)
        if not sprint_meta:
            print(
                f"[Baseline] WARNING: no sprint names contain '{increment}'. "
                "Falling back to date-window detection."
            )
            increment = None

    if not sprint_meta:
        pi_window = _get_pi_date_window(all_issues)
        if pi_window is None:
            raise ValueError("build_pi_baseline: could not determine PI date window.")
        pi_start, pi_end = pi_window
        sprint_meta = _collect_sprints(all_issues, pi_start, pi_end, team_name, None)
        if not sprint_meta and team_name:
            print(f"[Baseline] WARNING: no sprints found matching team '{team_name}'.")
            sprint_meta = _collect_sprints(all_issues, pi_start, pi_end, None, None)

    if not sprint_meta:
        raise ValueError("build_pi_baseline: no valid sprint data found.")

    sorted_sprints = sorted(sprint_meta.items(), key=lambda x: x[1]["sprint_start"])

    # ── 2. Assign each issue to its last sprint in this PI ────────────────────
    sprint_sp: dict[str, float] = {name: 0.0 for name, _ in sorted_sprints}
    carryover_sp,  carryover_count  = 0.0, 0   # sprints exist, none in this PI
    unassigned_sp, unassigned_count = 0.0, 0   # no sprint at all

    for item in all_issues:
        sp     = float(item.weight) if item.weight else 0.0
        target = _get_target_sprint_name(item.sprints, pi_start, pi_end, team_name, increment)
        if target in sprint_sp:
            sprint_sp[target] += sp
        elif item.sprints:
            carryover_sp    += sp
            carryover_count += 1
        else:
            unassigned_sp    += sp
            unassigned_count += 1

    # Drop sprints nothing resolved to (shadow / OBE sprints)
    sorted_sprints = [(n, m) for n, m in sorted_sprints if sprint_sp[n] > 0]
    if not sorted_sprints:
        raise ValueError(
            "build_pi_baseline: all sprints have 0 SP after filtering. "
            "Ensure issues have story_points and valid sprints in Neo4j."
        )

    total_sp = sum(sprint_sp[n] for n, _ in sorted_sprints)

    # ── 3. Sprint plan summary + spike detection ──────────────────────────────
    SPIKE_RATIO = 0.5
    spike_dates: list[datetime.datetime] = []
    plan_rows: list[dict] = []
    prior_sp = sprint_sp[sorted_sprints[0][0]]

    for name, meta in sorted_sprints:
        sp    = sprint_sp[name]
        days  = (meta["sprint_end"] - meta["sprint_start"]).days or 1
        spike = prior_sp > 0 and (sp - prior_sp) / prior_sp > SPIKE_RATIO
        if spike:
            spike_dates.append(meta["sprint_start"])
        plan_rows.append({
            "name":        name,
            "label":       _short_sprint_label(name, team_name, increment),
            "start":       meta["sprint_start"].date().isoformat(),
            "end":         meta["sprint_end"].date().isoformat(),
            "sp":          round(sp, 2),
            "pct":         round(sp / total_sp * 100, 2),
            "sp_per_week": round(sp / (days / 7), 2),
            "is_ip":       _is_ip_sprint(name),
            "spike":       spike,
        })
        prior_sp = sp

    sprint_plan = {
        "increment":        increment,
        "total_sp":         round(total_sp, 2),
        "pi_start":         plan_rows[0]["start"],
        "pi_end":           max(r["end"] for r in plan_rows),
        "sprints":          plan_rows,
        "carryover_sp":     round(carryover_sp, 2),
        "carryover_count":  carryover_count,
        "unassigned_sp":    round(unassigned_sp, 2),
        "unassigned_count": unassigned_count,
    }
    print(format_sprint_plan_text(sprint_plan))

    # ── 4. Emit one row per calendar day ──────────────────────────────────────
    baseline_rows: list[dict] = []
    cumulative_sp = 0.0

    for sprint_name, meta in sorted_sprints:
        sprint_start  = meta["sprint_start"]
        sprint_end    = meta["sprint_end"]
        sprint_days   = (sprint_end - sprint_start).days or 1
        sprint_target = sprint_sp[sprint_name]
        current_day   = sprint_start

        while current_day <= sprint_end:
            days_in = (current_day - sprint_start).days
            day_sp  = cumulative_sp + (days_in / sprint_days) * sprint_target
            baseline_rows.append({
                "date":         current_day,
                "sprint_name":  sprint_name,
                "baseline_sp":  round(day_sp, 2),
                "baseline_pct": round(day_sp / total_sp * 100, 2),
            })
            current_day += timedelta(days=1)

        cumulative_sp += sprint_target

    baseline_df = (
        pl.DataFrame(baseline_rows)
        .with_columns(pl.col("date").cast(pl.Datetime("us")))
        .sort("date")
    )
    return baseline_df, sprint_plan, spike_dates
