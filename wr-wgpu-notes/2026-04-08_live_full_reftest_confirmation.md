# Live Full Reftest Confirmation — 2026-04-08

## Status: 412/412 passing (current local Windows manifest/configuration)

This note records a direct live `wrench --wgpu reftest` run against the current
branch state after the P13/P14 notes.

Command shape used:

```powershell
Set-Location "c:\Users\mark_\Code\source\repos\webrender\wrench"
$env:RUST_LOG = "info"
& "C:\t\graphshell-target\debug\wrench.exe" --wgpu reftest `
  2>&1 | Tee-Object -FilePath "c:\Users\mark_\Code\source\repos\webrender\wr-wgpu-notes\logs\live_full_reftest_2026-04-08_run3.log"
```

Outcome:

- `REFTEST INFO | 412 passing, 0 failing`
- No `TEST-UNEXPECTED-FAIL` entries in the captured log
- The historically problematic cases from P13/P14 were observed passing during
  the live run, including `gradient_cache_clamp`, `snapshot-filters-01`, and
  `snapshot-shadow`

Primary artifact:

- [`logs/live_full_reftest_2026-04-08_run3.log`](logs/live_full_reftest_2026-04-08_run3.log)

## Interpretation

This result is better than the 434/441 status recorded in P13/P14. The most
likely explanation for the denominator difference is manifest/configuration
shape rather than regression in those notes: some diagnostics work had split
late failures into narrower jobs, while this run exercised the current local
Windows manifest as a single full pass.

The appropriate claim from this run is:

- the current local Windows `--wgpu reftest` manifest is clean
- the branch is materially ahead of the written P13/P14 progress notes
- downstream consumers can treat the backend as viable for native integration
  and engine bring-up, with cross-environment reconciliation as a separate
  documentation task rather than a blocker
