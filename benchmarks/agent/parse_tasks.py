#!/usr/bin/env python3
"""Read the flat tasks.yaml schema to JSON, avoiding a pyyaml dependency."""
import json
import sys


def strip_fold(value):
    """Drop a leading YAML folded-scalar marker (`>` / `>-`) as a prefix only."""
    v = value.strip()
    if v.startswith(">"):
        v = v[1:].lstrip("-")
    return v.strip()


tasks, cur, key = [], None, None
with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
    lines = fh.readlines()

i = 0
while i < len(lines):
    raw = lines[i].rstrip("\n")
    i += 1
    s = raw.strip()
    if not s or s.startswith("#") or s == "tasks:":
        continue
    if s.startswith("- id:"):
        if cur:
            tasks.append(cur)
        cur = {"id": s.split(":", 1)[1].strip(), "expected": []}
        key = None
        continue
    if cur is None:
        sys.exit(f"tasks.yaml: line {i} '{s}' appears before the first '- id:' entry")
    if s.startswith("prompt:"):
        val = strip_fold(s.split(":", 1)[1])
        block = []
        while i < len(lines) and lines[i].startswith("      ") and not lines[i].strip().startswith(("expected:", "- id:")):
            block.append(lines[i].strip())
            i += 1
        cur["prompt"] = " ".join([val] + block).strip()
        key = None
    elif s == "expected:":
        key = "expected"
    elif s.startswith("- ") and key == "expected":
        cur["expected"].append(s[2:].strip())

if cur:
    tasks.append(cur)
print(json.dumps(tasks))
