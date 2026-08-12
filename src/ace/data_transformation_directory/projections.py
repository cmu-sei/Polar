# ---------------------------------------------------------------------------
# projections.py
#
# PERT-based Monte Carlo projection engine.
#
# Branches from a user-supplied sprint_date using the actual completed_sp
# at that point as the floor, then samples N velocity paths forward to the
# PI end date.
#
# Velocity model (in priority order)
# ------------------------------------
# 1. training_df + remaining_issues supplied
#    → Issue-by-issue PERT fit: each remaining issue gets its own sampled
#      duration (working days) drawn from the historical distribution.
#      Issues complete individually as simulated working days elapse.
#      Use return_stats_df(..., recency_decay=0.5) for weighted training_df.
#      In this mode a per-issue completion-date summary is also returned.
#
# 2. training_df supplied without remaining_issues
#    → Daily-velocity PERT fit: a SP/day rate is sampled each day and
#      accumulated forward.  Equivalent to the old behaviour.
#
# 3. training_df is None
#    → Velocity derived from observed daily SP deltas in combined_df up to
#      sprint_date (non-zero days only). Useful when no historical data exists.
# ---------------------------------------------------------------------------

import datetime
from datetime import timedelta

import numpy as np
import math
import polars as pl


# ---------------------------------------------------------------------------
# PERT parameter fitting
# ---------------------------------------------------------------------------

TRAINING_COL = "days_per_story_point"
WEIGHT_COL   = "recency_weight"


def _fit_pert_params(
    training_df: pl.DataFrame,
    col:         str   = TRAINING_COL,
    p_low:       float = 0.10,
    p_high:      float = 0.90,
    scale:       int   = 5,
) -> tuple[float, float, float]:
    """
    Extract (a, b, c) PERT bounds from training_df[col].

    If training_df contains a `recency_weight` column (produced by
    training_projections._assign_recency_weights), rows are replicated
    proportional to their weight before percentiles are computed so that
    more-recent PI observations drive the distribution more than older ones.

    Parameters
    ----------
    col   : accrual column to fit — defaults to "days_per_story_point"
    p_low : optimistic bound percentile  (default P10)
    p_high: pessimistic bound percentile (default P90)
    scale : max replication factor for weighted mode (higher = more precision)

    Returns
    -------
    (a, b, c) where a = optimistic, b = most-likely, c = pessimistic
    """
    if col not in training_df.columns:
        raise ValueError(
            f"_fit_pert_params: column '{col}' not found in training_df. "
            f"Available columns: {training_df.columns}"
        )

    base = training_df.select(
        [c for c in [col, WEIGHT_COL] if c in training_df.columns]
    ).drop_nulls(subset=[col])

    if base.is_empty():
        raise ValueError(
            f"_fit_pert_params: no non-null values in '{col}' after dropping nulls."
        )

    if WEIGHT_COL in base.columns:
        max_w = base[WEIGHT_COL].max()
        if max_w and max_w > 0:
            base = base.with_columns(
                ((pl.col(WEIGHT_COL) / max_w * scale)
                 .round(0).cast(pl.Int32).clip(lower_bound=1))
                .alias("_rep")
            )
            values = (
                base
                .select(pl.col(col).repeat_by("_rep"))
                .explode(col)
                [col]
                .drop_nulls()
            )
            mode = "weighted"
        else:
            values = base[col]
            mode   = "unweighted (zero weights)"
    else:
        values = base[col]
        mode   = "unweighted"

    a = float(values.quantile(p_low))
    b = float(values.quantile(0.50))
    c = float(values.quantile(p_high))

    print(
        f"[PERT fit] {mode} | n={len(values)} | "
        f"a={a:.3f}  b={b:.3f}  c={c:.3f}  (col='{col}')"
    )
    return a, b, c


# ---------------------------------------------------------------------------
# remaining_issues field access
# ---------------------------------------------------------------------------

def _issue_field(iss, candidates: tuple[str, ...]):
    """
    Pull the first available field from a remaining_issues entry, supporting
    both dicts ({"key": ..., "sp": ...}) and objects (iss.name / iss.weight).
    """
    for c in candidates:
        if isinstance(iss, dict):
            if c in iss and iss[c] is not None:
                return iss[c]
        elif hasattr(iss, c):
            v = getattr(iss, c)
            if v is not None:
                return v
    raise ValueError(
        f"_issue_field: remaining_issues entry {iss!r} has none of {candidates}"
    )


# ---------------------------------------------------------------------------
# Per-issue completion-date summary
# ---------------------------------------------------------------------------

def _summarize_issue_completion_dates(
    comp_df:          pl.DataFrame,
    pi_end:           datetime.datetime,
    projection_count: int,
) -> pl.DataFrame:
    """
    Collapse one-row-per-(run, issue) completion dates into a per-issue
    summary: P10 / P50 / P90 projected completion dates plus the fraction of
    runs in which the issue finished on or before the PI end date.
    """
    summary = (
        comp_df
        .group_by("issue_key")
        .agg([
            pl.col("story_points").first(),
            pl.col("completion_date").cast(pl.Int64)
              .quantile(0.10, "nearest").cast(pl.Datetime("us"))
              .alias("p10_completion"),
            pl.col("completion_date").cast(pl.Int64)
              .quantile(0.50, "nearest").cast(pl.Datetime("us"))
              .alias("p50_completion"),
            pl.col("completion_date").cast(pl.Int64)
              .quantile(0.90, "nearest").cast(pl.Datetime("us"))
              .alias("p90_completion"),
            (pl.col("completion_date") <= pl.lit(pi_end).cast(pl.Datetime("us")))
              .mean().round(4)
              .alias("prob_done_by_pi_end"),
            pl.len().alias("n_runs"),
        ])
        .sort("p50_completion")
    )

    if (summary["n_runs"] != projection_count).any():
        print(
            "[Issue dates] WARNING: some issues have fewer samples than "
            f"projection_count={projection_count} — check remaining_issues "
            "for duplicate keys."
        )

    return summary.drop("n_runs")


# ---------------------------------------------------------------------------
# Handle Limited Team Size
# ---------------------------------------------------------------------------

def adjust_vector_for_team_size(team_size: int, durations_array: np.ndarray) -> np.ndarray:
    
    completions = np.asarray(durations_array, dtype=float).copy()
    pool = completions[:team_size].copy()   

    for i in range(team_size, len(completions)):
        j = int(pool.argmin())
        completions[i] += pool[j]
        pool[j] = completions[i]

    return completions


# ---------------------------------------------------------------------------
# Projection engine
# ---------------------------------------------------------------------------

def build_projections(
    combined_df:      pl.DataFrame,
    sprint_date:      str,
    total_sp:         float,
    projection_count: int                  = 500,
    training_df:      pl.DataFrame | None = None,
    training_col:     str                  = TRAINING_COL,
    team_size:        int                  = 7, 
    lam:              float                = 3.0,
    p_low:            float                = 0.10,
    p_high:           float                = 0.90,
    remaining_issues: list[dict] | None   = None,
) -> tuple[pl.DataFrame, pl.DataFrame | None]:
    """
    Generate N projection lines branching from sprint_date.

    Parameters
    ----------
    combined_df      : output of pipeline.dataframe_for_initial_graph()
    sprint_date      : "YYYY-MM-DD" — projections branch from this date
    total_sp         : PI planned scope (denominator for %)
    projection_count : number of Monte-Carlo runs
    training_df      : historical DataFrame from training_projections.return_stats_df()
                       Pass with recency_decay > 0 for weighted PERT fitting.
                       Pass None to derive velocity from current-PI actuals only.
    training_col     : which accrual column to fit the PERT distribution from
    lam              : PERT shape parameter λ — higher = tighter spread around b
    p_low / p_high   : optimistic / pessimistic velocity percentiles
    remaining_issues : list of dicts (or objects), one per incomplete issue at
                       sprint_date.  Each entry must expose:
                           key / name / issue_key       — issue identifier
                           sp  / weight / story_points  — story point value
                       When supplied, each issue receives its own PERT-sampled
                       duration (working days) and completes individually as
                       simulated working days elapse.  When None, the engine
                       falls back to the daily-velocity model.

    Returns
    -------
    (projections_df, issue_dates_df)

    projections_df : pl.DataFrame (long format) — columns:
        date, group ("Projection 1" … "Projection N"), completed_sp, completed_pct

    issue_dates_df : pl.DataFrame | None — only populated in issue-by-issue
        mode (remaining_issues supplied), otherwise None.  One row per
        outstanding issue:
        issue_key, story_points,
        p10_completion, p50_completion, p90_completion,  (projected dates)
        prob_done_by_pi_end                              (fraction of runs)
    """
    rng = np.random.default_rng()

    # ── 1. Locate branch point ────────────────────────────────────────────────
    if isinstance(sprint_date, str):
        sprint_dt = datetime.datetime.strptime(sprint_date, "%Y-%m-%d")
    else:
        sprint_dt = sprint_date
    sprint_dt = sprint_dt.replace(hour=0, minute=0, second=0, microsecond=0)

    branch_row = (
        combined_df
        .filter(pl.col("date") <= pl.lit(sprint_dt).cast(pl.Datetime("us")))
        .sort("date")
        .tail(1)
    )
    if branch_row.is_empty():
        raise ValueError(
            f"build_projections: sprint_date {sprint_date} is before any data in combined_df."
        )

    branch_sp  = float(branch_row["completed_sp"][0])
    branch_pct = float(branch_row["completed_pct"][0])

    # ── 2. Forward date spine: sprint_date → PI end ───────────────────────────
    pi_end = (
        combined_df
        .filter(pl.col("sprint_name").is_not_null())
        .sort("date")
        ["date"][-1]
        .replace(tzinfo=None)
    )

    n_days      = (pi_end - sprint_dt).days + 100
    future_days = [sprint_dt + timedelta(days=i) for i in range(n_days)]

    if n_days <= 0:
        raise ValueError(
            f"build_projections: sprint_date {sprint_date} is on or after PI end ({pi_end.date()})."
        )

    # Boolean mask: True for working days (Mon–Fri), False for weekends.
    # Bank holidays are not handled here — inject a custom mask if needed.
    workday_mask = np.array([d.weekday() < 5 for d in future_days], dtype=bool)

    # ── 3. Fit PERT velocity/duration distribution ────────────────────────────
    if training_df is not None:
        a_raw, b_raw, c_raw = _fit_pert_params(
            training_df, col=training_col, p_low=p_low, p_high=p_high
        )
    else:
        # Fallback: derive velocity from current-PI daily completions
        daily_deltas = (
            combined_df
            .filter(pl.col("date") <= pl.lit(sprint_dt).cast(pl.Datetime("us")))
            .sort("date")
            .with_columns(pl.col("completed_sp").diff().alias("delta"))
            .filter(pl.col("delta") > 0)
            ["delta"]
            .to_list()
        )
        if len(daily_deltas) < 3:
            raise ValueError(
                "build_projections: not enough completion days before "
                f"sprint_date ({sprint_date}) to fit a velocity model. "
                "Supply a training_df or choose a later sprint_date."
            )
        arr    = np.array(daily_deltas, dtype=float)
        a_raw  = float(np.percentile(arr, p_low  * 100))
        b_raw  = float(np.median(arr))
        c_raw  = float(np.percentile(arr, p_high * 100))
        print(
            f"[PERT fit] from current-PI actuals | n={len(arr)} | "
            f"a={a_raw:.3f}  b={b_raw:.3f}  c={c_raw:.3f}"
        )

    # Guard against degenerate distributions
    span = c_raw - a_raw
    if span <= 0:
        span  = 1e-9
        c_raw = a_raw + span

    # Converts a,b,c PERT paramaters to alpha and beta BETA dist paramaters

    alpha = max(1.0 + lam * (b_raw - a_raw) / span, 1e-12)
    beta_ = max(1.0 + lam * (c_raw - b_raw) / span, 1e-12)

    # ── 4. Run N projections ───────────────────────────────────────────────────
    projection_rows: list[dict] = []
    issue_dates_df:  pl.DataFrame | None = None

    if remaining_issues is not None:
        # ── Issue-by-issue mode ───────────────────────────────────────────────
        #
        # Each issue gets its own PERT-sampled duration (working days).
        # On every working day we advance elapsed time for all incomplete
        # issues by 1.  When an issue's elapsed days cross its sampled
        # duration it completes and its SP is added to the cumulative total.
        #
        # The inner loop is vectorized with NumPy:
        #   sampled_durations  shape (n_issues,) — one draw per issue
        #   elapsed            shape (n_issues,) — running working-day counter
        #   already_done       shape (n_issues,) — completion flag
        #
        # Sampling all issues in a single rng.beta call (shape n_issues)
        # mirrors the approach in build_projection_issue_map_beta and avoids
        # a Python-level loop over issues.
        #
        # Per-issue completion dates: an issue with sampled duration k
        # completes on the k-th working day strictly after sprint_dt — the
        # same semantics as the chart loop (elapsed starts incrementing on
        # the day after the branch point).  Rather than reading dates out of
        # the loop, we map k → calendar date directly with np.busday_offset,
        # which also captures issues whose sampled duration lands beyond the
        # chart horizon.
        # ─────────────────────────────────────────────────────────────────────
        n_issues   = len(remaining_issues)
        issue_keys = [
            str(_issue_field(iss, ("key", "name", "issue_key")))
            for iss in remaining_issues
        ]
        issue_sps  = np.array([
            float(_issue_field(iss, ("sp", "weight", "story_points")))
            for iss in remaining_issues
        ], dtype=float)

        # Working-day → calendar-date anchor.  base_day is the first business
        # day on/after the branch point; if sprint_dt itself is a business
        # day the k-th working day after it is base_day + k business days,
        # otherwise (weekend branch) the 1st working day after sprint_dt IS
        # base_day, so the shift is k - 1.
        start_np        = np.datetime64(sprint_dt.date())
        base_day        = np.busday_offset(start_np, 0, roll="forward")
        start_is_busday = bool(np.is_busday(start_np))

        completion_keys:   list[str]   = []
        completion_groups: list[str]   = []
        completion_sps:    list[float] = []
        completion_dates:  list        = []

        for i in range(projection_count):
            # One vectorized RNG call: sample a duration for every issue at once
            sampled_rate = a_raw + rng.beta(alpha, beta_, size=n_issues) * span
            temp_durations = np.ceil(sampled_rate * issue_sps)
            sampled_durations = adjust_vector_for_team_size(team_size, temp_durations)

            # ── Record projected completion dates for this run ───────────────
            k     = sampled_durations.astype(np.int64)
            shift = k if start_is_busday else np.maximum(k - 1, 0)
            run_completion_dates = np.busday_offset(base_day, shift)

            completion_keys.extend(issue_keys)
            completion_sps.extend(issue_sps.tolist())
            completion_groups.extend([f"Projection {i + 1}"] * n_issues)
            completion_dates.extend(
                run_completion_dates.astype("datetime64[us]").tolist()
            )

            elapsed      = np.zeros(n_issues, dtype=float)
            already_done = np.zeros(n_issues, dtype=bool)
            cumulative   = branch_sp

            projection_rows.append({
                    "date":          future_days[0],
                    "group":         f"Projection {i + 1}",
                    "completed_sp":  branch_sp,
                    "completed_pct": branch_pct,
                })

            max_working_days_needed = int(np.ceil(sampled_durations.max())) + 1
            run_end = sprint_dt + timedelta(days=max_working_days_needed * 2)
            run_days = [sprint_dt + timedelta(days=d) for d in range((run_end - sprint_dt).days + 1)]
            run_workday_mask = np.array([d.weekday() < 5 for d in run_days], dtype=bool)

            for day, is_workday in zip(future_days[1:], run_workday_mask[1:]):
                if is_workday:
                    elapsed += 1.0

                    # Issues that cross their duration threshold on this day
                    newly_done = ~already_done & (elapsed >= sampled_durations)
                    if newly_done.any():
                        cumulative   = min(cumulative + issue_sps[newly_done].sum(), total_sp)
                        already_done |= newly_done

                pct = round(cumulative / total_sp * 100, 2)
                pct = max(pct, branch_pct)   # floor: can't un-complete work
                pct = min(pct, 100.0)         # ceiling: can't exceed 100%

                projection_rows.append({
                    "date":          day,
                    "group":         f"Projection {i + 1}",
                    "completed_sp":  round(pct / 100 * total_sp, 2),
                    "completed_pct": pct,
                })

        # ── Per-issue completion-date summary across all runs ────────────────
        issue_completions_df = pl.DataFrame({
            "issue_key":       completion_keys,
            "story_points":    completion_sps,
            "group":           completion_groups,
            "completion_date": completion_dates,
        }).with_columns(pl.col("completion_date").cast(pl.Datetime("us")))

        issue_dates_df = _summarize_issue_completion_dates(
            issue_completions_df, pi_end, projection_count
        )

        print(
            f"[Issue dates] {n_issues} outstanding issues | "
            f"{projection_count} samples each | "
            f"{(issue_dates_df['prob_done_by_pi_end'] >= 0.5).sum()} "
            f"of {n_issues} more likely than not done by PI end "
            f"({pi_end.date()})"
        )
        print(issue_dates_df)

    else:
        # ── Daily-velocity mode (original behaviour) ──────────────────────────
        #
        # Sample one SP/day rate per day per run, accumulate forward.
        # remaining_issues was not supplied so we have no issue-level
        # granularity to work with — no per-issue dates are produced and
        # issue_dates_df is returned as None.
        # ─────────────────────────────────────────────────────────────────────
        print(
            f"[Projections] daily-velocity mode | "
            f"{projection_count} runs | {n_days} days"
        )

        for i in range(projection_count):
            daily_velocities = a_raw + rng.beta(alpha, beta_, size=n_days) * span
            cumulative = branch_sp

            projection_rows.append({
                    "date":          future_days[0],
                    "group":         f"Projection {i + 1}",
                    "completed_sp":  branch_sp,
                    "completed_pct": branch_pct,
            })

            for day, velocity, is_workday in zip(future_days[1:], daily_velocities, workday_mask[1:]):
                if is_workday:
                    cumulative = min(cumulative + velocity, total_sp)

                pct = round(cumulative / total_sp * 100, 2)
                pct = max(pct, branch_pct)
                pct = min(pct, 100.0)

                projection_rows.append({
                    "date":          day,
                    "group":         f"Projection {i + 1}",
                    "completed_sp":  round(pct / 100 * total_sp, 2),
                    "completed_pct": pct,
                })

    projections_df = (
        pl.DataFrame(projection_rows)
        .with_columns(pl.col("date").cast(pl.Datetime("us")))
        .sort(["group", "date"])
    )

    print(
        f"[Projections] complete | "
        f"branch: {branch_sp:.1f} SP ({branch_pct:.1f}%) on {sprint_date} | "
        f"forward: {n_days} days"
    )
    return projections_df, issue_dates_df