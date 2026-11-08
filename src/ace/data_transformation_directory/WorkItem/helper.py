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

"""This contains helper methods for use with building items."""
import datetime

import polars as pl


def _nth_weekday_of_month(year: int, month: int, weekday: int, n: int) -> datetime.date:
    """Return the date of the nth (1-indexed) given weekday in a month."""
    d = datetime.date(year, month, 1)
    offset = (weekday - d.weekday()) % 7
    d += datetime.timedelta(days=offset + 7 * (n - 1))
    return d


def _last_weekday_of_month(year: int, month: int, weekday: int) -> datetime.date:
    """Return the date of the last given weekday in a month."""
    if month == 12:
        next_month = datetime.date(year + 1, 1, 1)
    else:
        next_month = datetime.date(year, month + 1, 1)
    d = next_month - datetime.timedelta(days=1)
    offset = (d.weekday() - weekday) % 7
    return d - datetime.timedelta(days=offset)


def _observed(d: datetime.date) -> datetime.date:
    """Shift a holiday landing on a weekend to the nearest weekday, per
    the standard US federal holiday observance rule."""
    if d.weekday() == 5:   # Saturday -> observed Friday
        return d - datetime.timedelta(days=1)
    if d.weekday() == 6:   # Sunday -> observed Monday
        return d + datetime.timedelta(days=1)
    return d


def _us_federal_holidays(year: int) -> list[datetime.date]:
    """Return the observed dates of US federal holidays for a given year."""
    return [
        _observed(datetime.date(year, 1, 1)),                        # New Year's Day
        _nth_weekday_of_month(year, 1, 0, 3),                        # MLK Day (3rd Monday)
        _nth_weekday_of_month(year, 2, 0, 3),                        # Washington's Birthday (3rd Monday)
        _last_weekday_of_month(year, 5, 0),                          # Memorial Day (last Monday)
        _observed(datetime.date(year, 6, 19)),                       # Juneteenth
        _observed(datetime.date(year, 7, 4)),                        # Independence Day
        _nth_weekday_of_month(year, 9, 0, 1),                        # Labor Day (1st Monday)
        _nth_weekday_of_month(year, 10, 0, 2),                       # Columbus Day (2nd Monday)
        _observed(datetime.date(year, 11, 11)),                      # Veterans Day
        _nth_weekday_of_month(year, 11, 3, 4),                       # Thanksgiving (4th Thursday)
        _observed(datetime.date(year, 12, 25)),                      # Christmas Day
    ]


def get_us_federal_holidays(start: datetime.date, end: datetime.date) -> list[datetime.date]:
    """Return observed US federal holiday dates falling within [start, end]."""
    holidays: list[datetime.date] = []
    for year in range(start.year, end.year + 1):
        holidays.extend(_us_federal_holidays(year))
    return [d for d in holidays if start <= d <= end]


def is_late_story(df: pl.DataFrame, start_date_str: str) -> bool:
    # Parse the incoming date string once
    start_date = datetime.datetime.strptime(start_date_str, "%Y-%m-%d")

    # Only parse transition_time if it\'s still a String column
    if "transition_time" in df.columns and df["transition_time"].dtype == pl.String:
        df = df.with_columns(
            pl.col("transition_time")
              .str.strptime(pl.Datetime, strict=False)
              .dt.replace_time_zone(None)   # keep it tz-naive to match start_date
        )

    # Now transition_time is guaranteed to be Datetime (or nulls)
    return df.select(
        (pl.col("transition_time") > start_date).any()
    ).item()

WORK_START = datetime.time(8, 0)
WORK_END = datetime.time(17, 0)
WORK_SECS_PER_DAY = 9 * 3600  # 9 working hours per day


def _clamp_start_to_work(t: datetime.datetime) -> datetime.datetime:
    """Move forward to the next instant inside working hours (Mon-Fri, 8-17)."""
    # Skip weekends
    while t.weekday() >= 5:  # 5=Sat, 6=Sun
        t = datetime.datetime.combine(t.date() + datetime.timedelta(days=1),
                                      datetime.time(0, 0))

    # If after work hours, go to next working day at 8:00
    if t.time() >= WORK_END:
        d = t.date() + datetime.timedelta(days=1)
        t = datetime.datetime.combine(d, WORK_START)
        while t.weekday() >= 5:
            d = d + datetime.timedelta(days=1)
            t = datetime.datetime.combine(d, WORK_START)
        return t

    # If before work hours, clamp to 8:00 same day
    if t.time() < WORK_START:
        return datetime.datetime.combine(t.date(), WORK_START)

    return t


def _clamp_end_to_work(t: datetime.datetime) -> datetime.datetime:
    """Move backward to the previous instant inside working hours (Mon-Fri, 8-17)."""
    # Skip weekends
    while t.weekday() >= 5:
        t = datetime.datetime.combine(t.date() - datetime.timedelta(days=1),
                                      datetime.time(23, 59, 59))

    # If before work hours, go to previous working day at 17:00
    if t.time() <= WORK_START:
        d = t.date() - datetime.timedelta(days=1)
        t = datetime.datetime.combine(d, WORK_END)
        while t.weekday() >= 5:
            d = d - datetime.timedelta(days=1)
            t = datetime.datetime.combine(d, WORK_END)
        return t

    # If after work hours, clamp to 17:00 same day
    if t.time() > WORK_END:
        return datetime.datetime.combine(t.date(), WORK_END)

    return t


def _business_days_between(d1: datetime.date, d2: datetime.date) -> int:
    """Inclusive count of business days (Mon-Fri) between d1 and d2."""
    if d2 < d1:
        return 0
    days = (d2 - d1).days + 1
    full_weeks, extra = divmod(days, 7)
    weekdays = full_weeks * 5
    start_wd = d1.weekday()
    for i in range(extra):
        if (start_wd + i) % 7 < 5:
            weekdays += 1
    return weekdays


def get_work_hours_fast(start_time: datetime.datetime,
                        end_time: datetime.datetime) -> float:
    """
    Fast replacement: returns working hours between start_time and end_time,
    counting only Mon-Fri, 8:00-17:00.
    """

    # 1) Normalize to naive datetimes (strip timezone info)
    if isinstance(start_time, datetime.datetime) and start_time.tzinfo is not None:
        start_time = start_time.replace(tzinfo=None)
    if isinstance(end_time, datetime.datetime) and end_time.tzinfo is not None:
        end_time = end_time.replace(tzinfo=None)

    # 2) Now everything below works with naive datetimes only
    if end_time <= start_time:
        return 0.0

    # Clamp both endpoints into the working window
    s = _clamp_start_to_work(start_time)
    e = _clamp_end_to_work(end_time)

    if e <= s:
        return 0.0

    s_date = s.date()
    e_date = e.date()

    if s_date == e_date:
        # Same working day
        return max(0.0, (e - s).total_seconds()) / 3600.0

    # First day: from s to that day\'s 17:00
    first_end = datetime.datetime.combine(s_date, WORK_END)
    first_seconds = max(0.0, (first_end - s).total_seconds())

    # Last day: from that day\'s 8:00 to e
    last_start = datetime.datetime.combine(e_date, WORK_START)
    last_seconds = max(0.0, (e - last_start).total_seconds())

    # Full business days in between
    mid_start = s_date + datetime.timedelta(days=1)
    mid_end = e_date - datetime.timedelta(days=1)
    mid_seconds = 0.0
    if mid_end >= mid_start:
        mid_days = _business_days_between(mid_start, mid_end)
        mid_seconds = mid_days * WORK_SECS_PER_DAY

    return (first_seconds + last_seconds + mid_seconds) / 3600.0

def get_work_hours(start_time: datetime.datetime, end_time: datetime.datetime):
    """
    Backward-compatible wrapper around get_work_hours_fast.

    Returns (hours, holidays) so existing callers that expect holidays
    as the second element (and do len(result[1])) still work.
    """
    start_date = start_time.date() if isinstance(start_time, datetime.datetime) else start_time
    end_date   = end_time.date()   if isinstance(end_time, datetime.datetime)   else end_time
    holidays = get_us_federal_holidays(start_date, end_date)

    # Fast working-hours calculation (ignores holidays in the hours count,
    # as the original implementation did in practice).
    hours = get_work_hours_fast(start_time, end_time)

    return hours, holidays
