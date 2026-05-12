/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Roadmap A4 — per-frame timing diagnostics.
//!
//! Records named-span durations per render call. Captured on the
//! rasterizer side and surfaced via [`Renderer::last_frame_timings`]
//! for embedders who want to display per-phase costs (Vello encode,
//! tile invalidate, master compose, etc.) under load.
//!
//! The implementation is a thin wrapper over [`std::time::Instant`];
//! no `puffin` or other profiling-crate dep is required. If a future
//! consumer wants integration with a process-wide profiler, they can
//! drain `FrameTimings::spans` into the profiler's API and call this
//! module's spans the names of their choice.
//!
//! Design choices:
//!
//! - **`Vec<NamedSpan>` over named fields.** Adding a span (e.g., a
//!   future Vello-internal subspan) doesn't break the API.
//! - **Explicit `total` field.** Computed by an outer `Instant`
//!   span around the whole render. Decouples "wall-clock total" from
//!   the sum of `spans` so nested spans (added later) don't confuse
//!   the total semantics.
//! - **`&'static str` names.** No allocation per span; names are
//!   compile-time constants chosen by the recorder.

use std::time::{Duration, Instant};

/// One named timing span captured during a render call.
#[derive(Debug, Clone)]
pub struct NamedSpan {
    /// Identifier chosen by the recorder. Stable across calls;
    /// `'static` to avoid per-frame allocation.
    pub name: &'static str,
    /// Wall-clock duration of the span.
    pub duration: Duration,
}

/// Per-frame timing report captured by the rasterizer and surfaced
/// via [`crate::Renderer::last_frame_timings`].
///
/// `total` is the wall-clock duration of the whole render call.
/// `spans` is a list of sub-phases in record order. The sum of
/// `spans` may be **less than** `total` if some phases are not
/// instrumented, or **less than or equal to** `total` if spans are
/// non-overlapping. Nested spans (introduced for finer-grain
/// profiling later) may make the sum exceed `total`; trust `total`
/// for whole-frame cost, sum the spans you care about for sub-phase
/// cost.
#[derive(Debug, Clone)]
pub struct FrameTimings {
    /// Wall-clock duration of the whole render call.
    pub total: Duration,
    /// Sub-phase spans in record order.
    pub spans: Vec<NamedSpan>,
}

impl FrameTimings {
    /// Construct an empty `FrameTimings` for instrumentation.
    pub fn empty() -> Self {
        Self {
            total: Duration::ZERO,
            spans: Vec::new(),
        }
    }

    /// Append a named span. Used by the rasterizer's render path;
    /// public so embedders can inject their own spans into the same
    /// report (e.g., consumer-side compose work that wraps a
    /// netrender render call).
    pub fn record(&mut self, name: &'static str, duration: Duration) {
        self.spans.push(NamedSpan { name, duration });
    }

    /// Look up the duration for a span by name. Returns `None` if no
    /// span with that name was recorded this frame.
    pub fn span(&self, name: &str) -> Option<Duration> {
        self.spans
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.duration)
    }
}

/// Lightweight RAII-ish span helper. Construct at the start of a
/// phase, call [`Span::stop_recording`] at the end to push the span
/// into a `FrameTimings`.
///
/// Not `Drop`-based — explicit `stop_recording` lets the caller pick
/// the destination per-call instead of binding a single
/// `&mut FrameTimings` to the span's lifetime.
pub struct Span {
    name: &'static str,
    started: Instant,
}

impl Span {
    /// Start measuring a span named `name`.
    pub fn start(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
        }
    }

    /// Stop measuring and append to `timings`.
    pub fn stop_recording(self, timings: &mut FrameTimings) {
        let duration = self.started.elapsed();
        timings.record(self.name, duration);
    }

    /// Stop measuring without recording — returns the duration
    /// directly. Useful for spans whose result the caller wants to
    /// inspect inline.
    pub fn stop(self) -> Duration {
        self.started.elapsed()
    }
}
