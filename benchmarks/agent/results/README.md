# Agent-benchmark results

`run.sh` writes the latest run to `agent-latest.jsonl` (gitignored scratch —
**truncated at the start of every run**). Because a full real-repo run spends
real model tokens, archive any run worth keeping into a dated, committed file
here:

```bash
# after a run completes
cp agent-latest.jsonl "$(date +%Y-%m-%d)-repo-all-n6.jsonl"   # commit this
```

Committed archives (`*.jsonl`, plus any `*.md` write-up) are version-controlled;
only `agent-latest.jsonl` is ignored. Each archive should record the run config
(repos, `--runs`, model, date) so a number can be traced back to how it was
produced — see the per-run write-ups alongside the JSONL.
