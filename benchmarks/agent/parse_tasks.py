#!/usr/bin/env python3
"""Read the flat tasks.yaml schema to JSON, avoiding a pyyaml dependency."""
import json
import sys

tasks, cur, key = [], None, None
with open(sys.argv[1]) as fh:
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
    elif s.startswith("prompt:"):
        val = s.split(":", 1)[1].strip().lstrip(">").lstrip("-").strip()
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
