#!/usr/bin/env bash
set -euo pipefail

# Ensure the cartog index exists and is up to date.
# Run this at the start of a coding session.

DB_FILE=".cartog.db"

if [ ! -f "$DB_FILE" ]; then
    echo "No cartog index found. Building..."
    cartog index .
else
    echo "Updating cartog index..."
    cartog index .
fi
