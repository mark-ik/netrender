# WebRender wgpu Notes Index

This file is the short index for the surviving `wr-wgpu-notes/` set.

## Current Canonical Notes

- `2026-03-01_webrender_wgpu_renderer_implementation_plan.md`
  - main planning and current-state note
- `2026-04-08_live_full_reftest_confirmation.md`
  - latest direct full local reftest confirmation
- `archive/progress/2026-04-10_p15_progress_report.md`
  - wgpu 29 bump, inter-stage vars root cause, headless reftest mode
- `servo_wgpu_integration.md`
  - downstream Servo integration notes and host/device-sharing shape
- `shader_translation_journal.md`
  - WGSL translation history and workarounds

## Active Follow-up Plans

- `draw_context_plan.md`
- `typed_pipeline_metadata_plan.md`
- `texture_cache_cleanup_plan.md`
- `wasm-portability-checklist.md`

## Archive

- `archive/progress/`
  - dated progress snapshots kept for historical traceability
- `archive/legacy/`
  - older branch-shape notes, debug plans, and diagnostics writeups that are
    no longer the primary source of truth

## Local Logs

The `logs/` directory is intentionally local-only and gitignored except for its
`.gitignore` file. Keep only artifacts that still support an active note or an
unresolved diagnostic thread.
