import polars as pl
from datetime import datetime, timedelta
from statistics import median, mean, mode

from utils.neo4j_client import get_driver


def parse_datetime(record, key):
    datetime_str = record.get(key)
    if datetime_str is None:
        return None
    return datetime.strptime(datetime_str, "%Y-%m-%dT%H:%M:%S.%f%z").date()


def _detect_pi_window(
    sprint_dates: list[tuple[datetime, datetime]],
    gap_threshold_days: int = 21,
) -> tuple[datetime, datetime] | None:
    """
    Given a list of (sprint_start, sprint_end) pairs (naive datetimes),
    return the (pi_start, pi_end) of the densest contiguous cluster.

    Mirrors the logic in baseline._get_pi_date_window:
      - Sort sprints by end date.
      - Walk the sorted end dates; when the gap between consecutive end dates
        exceeds gap_threshold_days, start a new cluster.
      - The largest cluster is the active PI.  Its pi_start is the earliest
        sprint_start within the cluster; pi_end is the latest sprint_end.

    Returns None if sprint_dates is empty.
    """
    if not sprint_dates:
        return None

    # Sort by sprint end date so we can detect temporal gaps
    sorted_sprints = sorted(sprint_dates, key=lambda x: x[1])
    end_dates      = [s[1] for s in sorted_sprints]

    # Cluster by gap between consecutive end dates
    clusters: list[list[int]] = []   # each inner list = indices into sorted_sprints
    current_cluster: list[int] = [0]

    for i in range(1, len(end_dates)):
        gap = (end_dates[i] - end_dates[i - 1]).days
        if gap > gap_threshold_days:
            clusters.append(current_cluster)
            current_cluster = [i]
        else:
            current_cluster.append(i)
    clusters.append(current_cluster)

    # Pick the largest cluster (most sprints = active PI)
    best_cluster = max(clusters, key=len)

    pi_start = min(sorted_sprints[i][0] for i in best_cluster)
    pi_end   = max(sorted_sprints[i][1] for i in best_cluster)

    return pi_start, pi_end


def _sprint_belongs_to_team(sprint_name: str, team_name: str) -> bool:
    """
    Return True if the sprint name appears to belong to the given team.
    Matching is case-insensitive and checks the start of the name so that
    "Thestrals PI-29 J" matches "Thestrals" but "Keymasters 2025 J" does not.
    """
    return sprint_name.lower().startswith(team_name.lower())


def get_earliest_and_latest_date(team_name: str, increment_id: str, stories: bool):
    driver = get_driver()

    type_match  = "MATCH (i:JiraIssue)-[:HAS_TYPE]->(it:JiraIssueType)" if stories else ""
    type_filter = "AND it.name = 'Story'"                        if stories else ""

    all_sprints_query = f"""
        MATCH (t:JiraIssue_Agile_Team)<-[:HAS_AGILE_TEAM]-(i:JiraIssue)
        MATCH (i)-[:HAS_PROGRAM_INCREMENT]->(pi:JiraIssue_Program_Increment)
        MATCH (i)-[:HAS_SPRINT]->(s:Sprint)
        {type_match}
        WHERE t.value = $team_name
          AND pi.value = $increment_id
          AND s.startDate IS NOT NULL
          AND s.endDate IS NOT NULL
          AND s.name IS NOT NULL
          {type_filter}
        RETURN DISTINCT s.startDate AS sprint_start, s.endDate AS sprint_end, s.name AS sprint_name
    """

    with driver.session() as session:
        records = session.run(
            all_sprints_query,
            team_name=team_name,
            increment_id=increment_id,
        ).data()

    if not records:
        return None, None

    sprint_dates: list[tuple[datetime, datetime]] = []
    for row in records:
        sprint_name = row.get("sprint_name") or ""
        if not _sprint_belongs_to_team(sprint_name, team_name):
            continue
        if increment_id.lower() not in sprint_name.lower():
            continue
        try:
            start = datetime.strptime(str(row["sprint_start"])[:10], "%Y-%m-%d")
            end   = datetime.strptime(str(row["sprint_end"])[:10],   "%Y-%m-%d")
            sprint_dates.append((start, end))
        except (ValueError, TypeError):
            continue

    if not sprint_dates:
        return None, None

    pi_window = _detect_pi_window(sprint_dates)
    if pi_window is None:
        return None, None

    pi_start, pi_end = pi_window

    pi_sprints = [(s, e) for s, e in sprint_dates if pi_start <= e <= pi_end]
    earliest   = min(s for s, _ in pi_sprints).date()
    latest     = max(e for _, e in pi_sprints).date()

    return earliest, latest


def get_projection_start_date(team_name: str, increment_id: str, stories: bool):

    _, latest = get_earliest_and_latest_date(team_name, increment_id, stories)
    return min(latest, datetime.today().date())
