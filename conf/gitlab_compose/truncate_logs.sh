#!/bin/bash

# Define the directory to search and the size to truncate to
DIRECTORY="./gitlab_logs"
SIZE="1k"  # Keeps last 10KB of each log

# Find all .log files recursively in the directory
LOG_FILES=$(find "$DIRECTORY" -type f -name "*")

# Iterate over each file in the results
for file in $LOG_FILES; do
    # Use tail to get the last SIZE of the file, then overwrite the file
    tail -c $SIZE "$file" > "$file.tmp" && mv "$file.tmp" "$file"
done

echo "Logs have been truncated."
