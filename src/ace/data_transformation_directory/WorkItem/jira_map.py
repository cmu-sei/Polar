"""Holds the business logic for dealing with Jira."""

# The jira label we're concerned about for active work
IN_PROGRESS_STATE = 'In Progress'

# Ones we're concerned with
STATE_LIST = ['Planned', 'In Progress', 'Verifying', 'Validating', 'Acceptance', 'Done']
