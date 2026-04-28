---
name: wgpu backend for WebRender (experimental branch)
description: Working project note for the experimental wgpu backend branch, including seam exploration and larger refactor ideas that are intentionally out of scope for the minimal branch.
type: project
---

# WebRender wgpu Backend Experimental Track

This note belongs to the **experimental** branch, not the minimal additive branch.

## Branch

Use this note for work on the experimental branch line, where broader renderer/device seam changes are allowed.

The minimal additive proof branch is tracked separately in:

- `wgpu-backend-minimal-plan.md`

## Purpose

This track exists to explore the larger architectural questions that the minimal branch intentionally avoids:

- what renderer-side seam is eventually appropriate for a real non-GL backend?
- what parts of `Device` should become backend-owned?
- how much GL-shaped state can be isolated without making the renderer worse?
- what abstractions survive contact with a real backend implementation?
- does `wgpu-hal` materially change what the right seam looks like?

This is where it is acceptable to experiment more freely, including:

- `RendererBackend`
- `GpuDevice`
- `wgpu-hal`
- backend-owned device construction
- renderer/device seam extraction
- narrower or broader refactors in startup/config/device ownership

## Relationship To The Minimal Branch

The minimal branch should stay reviewer-friendly and proof-oriented:

- leave GL behavior alone
- add `wgpu_backend` additively
- keep GL as the default selected backend
- prove real rendering works
- validate through Servo / Graphshell

This experimental branch can build on lessons from that proof, but it should not be treated as the default review path for landing the initial backend experiment.

In other words:

- minimal branch = prove the backend works with minimal scope
- experimental branch = investigate what a cleaner long-term renderer/backend seam and/or `wgpu-hal` implementation might look like

## Current Guidance

When working on this branch:

- prefer experiments that answer a concrete architectural question
- do not assume every experiment belongs in the minimal branch
- keep notes honest about what is exploratory versus what is required
- mine the minimal branch for proven backend behavior before introducing larger refactors here

## Questions This Branch Should Answer

Examples:

1. Is a renderer-level `RendererBackend` enum actually the right public seam?
2. Is a single `GpuDevice` trait viable, or does it force `wgpu` to pretend to be GL?
3. Which renderer subsystems are easiest to make backend-neutral without excessive churn?
4. What is the smallest integration seam needed once the additive backend proof is downstream-validated?
5. Which GL-only paths should remain explicitly GL-owned even in a future dual-backend design?
6. Does `wgpu-hal` reduce impedance enough to justify a different backend shape than the minimal branch uses?

## Suggested Working Rules

- prefer small experimental commits with clear intent
- separate exploratory refactors from backend-proof work
- if a change would materially increase reviewer burden for the minimal branch, keep it here
- if a change is only useful after renderer wiring begins, it likely belongs here rather than in the minimal branch
- if `wgpu-hal` pushes the design toward a different seam, record that difference explicitly instead of silently back-porting assumptions to the minimal branch

## Shared References

- `wgpu-backend-minimal-plan.md`
  - current additive proof plan
- `shader_translation_journal.md`
  - detailed WGSL translation record

## Status

This note is intentionally lightweight again.

The older seam-first plan that previously lived here represented one exploratory direction, but it should no longer be treated as the canonical plan for the minimal 0.68 backend proof.

Current working assumption:

- minimal branch uses `wgpu` and the smallest viable `RendererBackend` / `GpuDevice` surface
- experimental branch is the place to test whether `wgpu-hal` changes the backend implementation story enough to justify a different long-term seam
