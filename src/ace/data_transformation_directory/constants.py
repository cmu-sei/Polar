# ---------------------------------------------------------------------------
# constants.py
#
# Shared constants used across the PI dashboard pipeline.
# Add new terminal Jira states here — all modules pick them up automatically.
# ---------------------------------------------------------------------------

DONE_STATES: set[str] = {
    "Done",
    "Closed",
    "Cancelled",
}
