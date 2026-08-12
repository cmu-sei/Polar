
# ---------------------------------------------------------------------------
# baseline.py
#
# Builds the piecewise-linear planned baseline for a PI.
# Each sprint gets its own slope proportional to the SP assigned to it,
# producing a staircase-of-ramps rather than a single diagonal.
# ---------------------------------------------------------------------------

import datetime
from datetime import timedelta

import polars as pl

from data_transformation_directory.Db import _normalize_to_naive, get_data_baseline


# ---------------------------------------------------------------------------
# Sprint name helpers
# ---------------------------------------------------------------------------

def _clean_sprint_name(name: str) -> str:
    """Strip the rapidViewId suffix Jira appends to sprint names."""
    return name.split(",rapidViewId=")[0].strip() if name else name


def _get_pi_date_window(all_issues: list) -> tuple[datetime.datetime, datetime.datetime] | None:
    """
    Derive the PI date window by finding the tightest cluster of sprint dates.

    Issues can be associated with sprints from multiple PIs (e.g. carried-over
    issues still reference old OBE sprints).  Rather than trusting every sprint
    date naively, we collect all sprint end-dates, sort them, and treat the
    densest cluster as the active PI.  Sprints whose end-date falls more than
    one sprint-length (≈ 14 days) outside that cluster are treated as
    cross-PI noise and excluded.

    Returns (pi_start, pi_end) as naive datetimes, or None if insufficient data.
    """
    all_ends: list[datetime.datetime] = []
    for item in all_issues:
        for s in item.sprints:
            end = _normalize_to_naive(s.get("endDate"))
            if end is not None:
                all_ends.append(end)

    if len(all_ends) < 2:
        return None

    all_ends.sort()

    # Find the largest contiguous group of sprint end-dates where consecutive
    # dates are no more than 21 days apart (one sprint + small buffer).
    # This is robust to a few outlier sprints from other PIs.
    GAP_THRESHOLD = timedelta(days=21)
    best_start_idx = 0
    best_len       = 1
    cur_start_idx  = 0

    for i in range(1, len(all_ends)):
        if (all_ends[i] - all_ends[i - 1]) > GAP_THRESHOLD:
            if (i - cur_start_idx) > best_len:
                best_len       = i - cur_start_idx
                best_start_idx = cur_start_idx
            cur_start_idx = i

    # Check the final segment
    if (len(all_ends) - cur_start_idx) > best_len:
        best_start_idx = cur_start_idx

    cluster_end   = all_ends[-1] if cur_start_idx == best_start_idx else all_ends[best_start_idx + best_len - 1]
    cluster_start = all_ends[best_start_idx]

    # Expand the window slightly to capture sprint start dates that precede
    # the first sprint's end date.
    pi_start = cluster_start - timedelta(days=21)
    pi_end   = cluster_end   + timedelta(days=1)

    return pi_start, pi_end


def _sprint_belongs_to_team(sprint_name: str, team_name: str) -> bool:
    """
    Return True if the sprint name appears to belong to the given team.
    Matching is case-insensitive and checks the start of the name so that
    "Thestrals PI-29 J" matches "Thestrals" but "Keymasters 2025 J" does not.
    """
    return sprint_name.lower().startswith(team_name.lower())


def _get_target_sprint_name(
    sprints:   list[dict],
    pi_start:  datetime.datetime,
    pi_end:    datetime.datetime,
    team_name: str | None = None,
) -> str | None:
    """
    Return the cleaned name of the LAST sprint an issue is assigned to
    within the current PI window (by sprint end date).

    Filters applied (in order):
      1. Sprint end date must fall within [pi_start, pi_end].
      2. If team_name is supplied, sprint name must start with that team name
         (case-insensitive) — cross-team sprints like "Keymasters 2025 J"
         are excluded even when they carry real SP.
    """
    candidates = []
    for s in sprints:
        end  = _normalize_to_naive(s.get("endDate"))
        name = _clean_sprint_name(s.get("name", ""))
        if end is None:
            continue
        if not (pi_start <= end <= pi_end):
            continue
        if team_name and not _sprint_belongs_to_team(name, team_name):
            continue
        candidates.append((end, name))

    return max(candidates, key=lambda x: x[0])[1] if candidates else None


# ---------------------------------------------------------------------------
# Baseline construction
# ---------------------------------------------------------------------------


def build_pi_baseline(all_issues: list, team_name: str | None = None) -> pl.DataFrame:
    """
    Build a piecewise-linear baseline (0% → 100%) across the PI.

    How it works
    ------------
    1. The active PI date window is derived automatically by finding the
       densest cluster of sprint end-dates, excluding sprints from other PIs
       that issues may still reference (e.g. carried-over OBE sprints).
    2. Sprint names are filtered to those belonging to `team_name` so that
       cross-team sprints (e.g. "Keymasters 2025 J") are excluded even when
       they carry real SP.  Falls back to all sprints if no team match found.
    3. Each issue is assigned to its TARGET sprint — the last team-owned
       sprint it appears in within the PI window.
    4. SP are summed per sprint to produce each sprint's planned workload.
    5. Within each sprint the baseline ramps linearly from the cumulative
       total at sprint-start to the cumulative total at sprint-end.
    6. Sprints chain: Sprint B starts exactly where Sprint A ended.

    Parameters
    ----------
    all_issues : list[BaseItem]
        Each item must expose .weight (float) and .sprints (list of dicts
        with keys startDate, endDate, name).
    team_name  : team name string as it appears at the start of sprint names
                 (e.g. "Thestrals").  None = no team filtering (old behaviour).

    Returns
    -------
    pl.DataFrame — columns: date, sprint_name, baseline_sp, baseline_pct
    """

    # ── 0. Derive PI date window ──────────────────────────────────────────────
    pi_window = _get_pi_date_window(all_issues)
    if pi_window is None:
        raise ValueError(
            "build_pi_baseline: could not determine PI date window. "
            "Check that sprint endDate is populated in Neo4j."
        )
    pi_start, pi_end = pi_window

    # ── 1. Build canonical sprint table (PI window + team filter) ─────────────
    sprint_meta: dict[str, dict] = {}

    for item in all_issues:
        for s in item.sprints:
            name  = _clean_sprint_name(s.get("name", ""))
            start = _normalize_to_naive(s.get("startDate"))
            end   = _normalize_to_naive(s.get("endDate"))
            if not (name and start is not None and end is not None):
                continue
            if not (pi_start <= end <= pi_end):
                continue
            if team_name and not _sprint_belongs_to_team(name, team_name):
                continue
            sprint_meta.setdefault(name, {"sprint_start": start, "sprint_end": end})

    # Graceful fallback: if team filtering wiped everything (unexpected naming),
    # retry without the team filter and warn so the caller knows.
    if not sprint_meta and team_name:
        print(
            f"[Baseline] WARNING: no sprints found matching team '{team_name}'. "
            "Falling back to all sprints within PI window."
        )
        for item in all_issues:
            for s in item.sprints:
                name  = _clean_sprint_name(s.get("name", ""))
                start = _normalize_to_naive(s.get("startDate"))
                end   = _normalize_to_naive(s.get("endDate"))
                if name and start is not None and end is not None:
                    if pi_start <= end <= pi_end:
                        sprint_meta.setdefault(name, {"sprint_start": start, "sprint_end": end})

    if not sprint_meta:
        raise ValueError(
            "build_pi_baseline: no valid sprint data found within PI window. "
            "Check that sprint startDate / endDate are populated in Neo4j."
        )

    sorted_sprints = sorted(sprint_meta.items(), key=lambda x: x[1]["sprint_start"])

    # ── 2. Sum SP per sprint ──────────────────────────────────────────────────
    sprint_sp: dict[str, float] = {name: 0.0 for name, _ in sorted_sprints}
    unassigned_sp = 0.0

    for item in all_issues:
        sp     = float(item.weight) if item.weight else 0.0
        target = _get_target_sprint_name(item.sprints, pi_start, pi_end, team_name)
        if target in sprint_sp:
            sprint_sp[target] += sp
        else:
            unassigned_sp += sp
    '''
    if sorted_sprints and unassigned_sp > 0:
        sprint_sp[sorted_sprints[-1][0]] += unassigned_sp
    '''
    # ── Drop zero-SP sprints ──────────────────────────────────────────────────
    # Sprints with 0 SP are concurrently-running OBE/shadow sprints that issues
    # reference in their history but never resolved to as their target sprint.
    # They contribute nothing to the baseline slope and produce duplicate/flat
    # segments, so remove them before chaining. Cumulative total is unaffected.
    sorted_sprints = [(name, meta) for name, meta in sorted_sprints if sprint_sp[name] > 0]

    if not sorted_sprints:
        raise ValueError(
            "build_pi_baseline: all sprints have 0 SP after filtering. "
            "Ensure issues have story_points and valid target sprints in Neo4j."
        )

    total_sp = sum(sprint_sp[name] for name, _ in sorted_sprints)

    if total_sp == 0:
        raise ValueError(
            "build_pi_baseline: total story points is 0. "
            "Ensure issues have story_points populated in Neo4j."
        )

    return_string = (
            f"{len(sorted_sprints)} sprints | Total SP: {total_sp:.1f}\n"
        )
    
    SPIKE_RATIO = .5
    spike_dates: list[datetime.datetime] = []
    prior_sp = sprint_sp[sorted_sprints[0][0]]

    for name, meta in sorted_sprints:
            sp = sprint_sp[name]
            weeks = (meta['sprint_end'] - meta['sprint_start']).days / 7
            return_string += (
                f"  {name}  "
                f"{meta['sprint_start'].date()} → {meta['sprint_end'].date()}"
                f"  |  {sp:.1f} SP ({sp / total_sp * 100:.1f}%)"
                f"  |  {sp / weeks:.1f} SP/week\n"
            )
            if prior_sp > 0 and ((sp - prior_sp) / prior_sp) > SPIKE_RATIO:
                spike_dates.append(meta['sprint_start'])
            prior_sp = sp

    if unassigned_sp > 0:
        return_string += (
            f"\nUnassigned (no sprint):  {unassigned_sp:.1f} SP  "
            f"— excluded from baseline\n"
            f"    Planned baseline reflects sprint-committed work only ({total_sp:.1f} SP)."
        )

    print("spike date")
    print(spike_dates)

    return_string = return_string.rstrip("\n")

    # ── 3. Emit one row per calendar day ──────────────────────────────────────
    baseline_rows: list[dict] = []
    cumulative_sp = 0.0

    for sprint_name, meta in sorted_sprints:
        sprint_start  = meta["sprint_start"]
        sprint_end    = meta["sprint_end"]
        sprint_days   = (sprint_end - sprint_start).days or 1
        sprint_target = sprint_sp[sprint_name]
        sp_at_start   = cumulative_sp
        current_day   = sprint_start

        while current_day <= sprint_end:
            days_in        = (current_day - sprint_start).days
            day_sp         = sp_at_start + (days_in / sprint_days) * sprint_target
            baseline_rows.append({
                "date":         current_day,
                "sprint_name":  sprint_name,
                "baseline_sp":  round(day_sp, 2),
                "baseline_pct": round(day_sp / total_sp * 100, 2),
            })
            current_day += timedelta(days=1)

        cumulative_sp += sprint_target

    return (
        pl.DataFrame(baseline_rows)
        .with_columns(pl.col("date").cast(pl.Datetime("us")))
        .sort("date")
    ), return_string, spike_dates
