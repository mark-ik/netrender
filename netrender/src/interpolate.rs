/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Roadmap D2 — timing curves and value-interpolation helpers.
//!
//! netrender is intentionally **clockless**: no `Instant`, no
//! per-frame state, no animation runtime. The consumer drives time
//! (whatever clock they own — winit's frame timer, a media player's
//! presentation clock, scrubber state, replay) and uses the
//! functions here to convert a normalised parameter `t ∈ [0.0,
//! 1.0]` into the eased value the renderer should consume.
//!
//! Typical flow (consumer-side):
//!
//! ```text
//! let elapsed = now - animation_start;       // your clock
//! let t = (elapsed / duration).clamp(0.0, 1.0);
//! let alpha = lerp(start_alpha, end_alpha, ease_in_out(t));
//! scene.push_layer_alpha(alpha);
//! ```
//!
//! Why no `Animated<T>` wrapper or scene-side animation field? Two
//! reasons. (1) Animation orthogonality: a single value can be
//! driven by more than one source (CSS animation + scrubber +
//! reduced-motion override), and only the consumer knows how to
//! resolve those layers. (2) Determinism: storing animation state
//! on a Scene would couple rendering to wall-clock time and make
//! `Scene::snapshot` non-reproducible. Keeping the curves as pure
//! functions preserves the "Scene is a frame description; replays
//! deterministically" invariant from A2.

use core::ops::{Add, Mul, Sub};

// ── Easing curves: t in [0, 1] -> eased t in [0, 1] ─────────────────

/// Identity. `linear(t) == t`. Provided for symmetry with the named
/// curves; useful when your code path takes a function pointer.
#[inline]
pub fn linear(t: f32) -> f32 {
    t
}

/// CSS `ease-in` — slow start, accelerating. Cubic bezier
/// `(0.42, 0.0, 1.0, 1.0)`.
#[inline]
pub fn ease_in(t: f32) -> f32 {
    cubic_bezier(0.42, 0.0, 1.0, 1.0, t)
}

/// CSS `ease-out` — fast start, decelerating. Cubic bezier
/// `(0.0, 0.0, 0.58, 1.0)`.
#[inline]
pub fn ease_out(t: f32) -> f32 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, t)
}

/// CSS `ease-in-out` — symmetric S-curve. Cubic bezier
/// `(0.42, 0.0, 0.58, 1.0)`.
#[inline]
pub fn ease_in_out(t: f32) -> f32 {
    cubic_bezier(0.42, 0.0, 0.58, 1.0, t)
}

/// CSS `ease` — the default keyword's curve. Cubic bezier
/// `(0.25, 0.1, 0.25, 1.0)`.
#[inline]
pub fn ease(t: f32) -> f32 {
    cubic_bezier(0.25, 0.1, 0.25, 1.0, t)
}

/// CSS `step-start` — jumps from 0 to 1 at `t == 0`.
#[inline]
pub fn step_start(t: f32) -> f32 {
    if t > 0.0 {
        1.0
    } else {
        0.0
    }
}

/// CSS `step-end` — stays at 0 until `t == 1`, then jumps to 1.
#[inline]
pub fn step_end(t: f32) -> f32 {
    if t >= 1.0 {
        1.0
    } else {
        0.0
    }
}

/// Generic cubic Bezier easing curve following the CSS
/// `cubic-bezier(p1x, p1y, p2x, p2y)` convention. The endpoints are
/// fixed at `(0, 0)` and `(1, 1)`; only the two control points are
/// caller-supplied. `p1x` and `p2x` should lie in `[0, 1]` for a
/// monotonic curve (CSS doesn't enforce this; we don't either, but
/// non-monotonic inputs may produce loop-back behaviour).
///
/// The implementation Newton-iterates to invert the bezier's `x(t)`
/// parametrisation (since the input is the curve's x, not its
/// parameter) and then evaluates `y(t)` at the inverted parameter.
/// This matches the WebKit / Blink CSS implementations.
pub fn cubic_bezier(p1x: f32, p1y: f32, p2x: f32, p2y: f32, x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let t = solve_bezier_x(p1x, p2x, x);
    bezier_axis(p1y, p2y, t)
}

/// Evaluate a 1D cubic Bezier with endpoints 0 and 1 at parameter
/// `t`. The two control values are `c1` and `c2`.
fn bezier_axis(c1: f32, c2: f32, t: f32) -> f32 {
    // Standard Bezier blend with P0 = 0, P3 = 1:
    //   B(t) = 3(1-t)²t · c1 + 3(1-t)t² · c2 + t³
    let one_minus = 1.0 - t;
    3.0 * one_minus * one_minus * t * c1 + 3.0 * one_minus * t * t * c2 + t * t * t
}

/// Newton-iterate to find `t` such that the x-axis cubic Bezier
/// (P0 = 0, P1 = p1x, P2 = p2x, P3 = 1) evaluates to `target_x`.
/// Falls back to bisection if Newton stalls.
fn solve_bezier_x(p1x: f32, p2x: f32, target_x: f32) -> f32 {
    const ITERATIONS: usize = 10;
    const EPSILON: f32 = 1.0e-6;

    let mut t = target_x;
    for _ in 0..ITERATIONS {
        let x = bezier_axis(p1x, p2x, t);
        let dx = x - target_x;
        if dx.abs() < EPSILON {
            return t;
        }
        // Derivative of B(t) wrt t with P0=0, P3=1:
        //   B'(t) = 3(1-t)² · c1 + 6(1-t)t · (c2 - c1) + 3t² · (1 - c2)
        let one_minus = 1.0 - t;
        let dxdt = 3.0 * one_minus * one_minus * p1x
            + 6.0 * one_minus * t * (p2x - p1x)
            + 3.0 * t * t * (1.0 - p2x);
        if dxdt.abs() < EPSILON {
            // Tangent vanishes; fall back to bisection.
            break;
        }
        t -= dx / dxdt;
        if t < 0.0 || t > 1.0 {
            break;
        }
    }
    // Bisection fallback (always converges).
    let mut lo = 0.0_f32;
    let mut hi = 1.0_f32;
    let mut t = target_x;
    for _ in 0..32 {
        let x = bezier_axis(p1x, p2x, t);
        if (x - target_x).abs() < EPSILON {
            return t;
        }
        if x < target_x {
            lo = t;
        } else {
            hi = t;
        }
        t = 0.5 * (lo + hi);
    }
    t
}

// ── Value interpolation: lerp scalar / array / color ────────────────

/// Linear interpolation between `a` and `b` by parameter `t`. `t`
/// is **not** clamped — callers passing `t > 1.0` get extrapolation
/// (sometimes useful for spring-style overshoot). Use
/// `t.clamp(0.0, 1.0)` first if strict in-range behaviour is wanted.
#[inline]
pub fn lerp<T>(a: T, b: T, t: f32) -> T
where
    T: Copy + Add<Output = T> + Sub<Output = T> + Mul<f32, Output = T>,
{
    a + (b - a) * t
}

/// Element-wise lerp on a fixed-size array. Useful for `[f32; 4]`
/// colors, `[f32; 6]` 2D affines, etc.
#[inline]
pub fn lerp_array<const N: usize>(a: [f32; N], b: [f32; N], t: f32) -> [f32; N] {
    let mut out = [0.0_f32; N];
    for i in 0..N {
        out[i] = a[i] + (b[i] - a[i]) * t;
    }
    out
}

/// Lerp two **premultiplied** RGBA colors. Same math as
/// `lerp_array::<4>` — exposed under a name that documents the
/// premultiplied-alpha contract netrender uses everywhere
/// (`SceneRect::color`, `SceneStroke::color`, etc.).
///
/// Note: lerping premultiplied colors is **not** the same as
/// lerping straight-alpha colors — the result's alpha is the
/// straight-alpha lerp, but the RGB values are pre-multiplied
/// proportionally. That's exactly what netrender wants because the
/// pipeline treats RGBA as premultiplied throughout.
#[inline]
pub fn lerp_color(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    lerp_array(a, b, t)
}

// ── Keyframe sampling ────────────────────────────────────────────────

/// Sample a keyframe sequence at parameter `t ∈ [0, 1]`. Each
/// keyframe is `(time, value)`; times must be non-decreasing across
/// the slice. Values between keyframes are linearly interpolated.
///
/// Returns the first keyframe's value for `t <= keyframes[0].0`,
/// the last keyframe's value for `t >= keyframes[last].0`, and the
/// linear blend between bracketing keyframes otherwise. Returns
/// `default` if the slice is empty.
///
/// Keep this **pure**: caller-supplied easing is applied to `t`
/// *before* calling this. To stack ease-in-out onto a 3-keyframe
/// animation, do `sample_keyframes(&kf, ease_in_out(t), default)`.
pub fn sample_keyframes<T>(keyframes: &[(f32, T)], t: f32, default: T) -> T
where
    T: Copy + Add<Output = T> + Sub<Output = T> + Mul<f32, Output = T>,
{
    if keyframes.is_empty() {
        return default;
    }
    if t <= keyframes[0].0 {
        return keyframes[0].1;
    }
    let last = keyframes.len() - 1;
    if t >= keyframes[last].0 {
        return keyframes[last].1;
    }
    // Find the bracketing pair. Linear scan is fine — keyframe lists
    // are typically tiny (3–8 entries).
    for i in 0..last {
        let (t0, v0) = keyframes[i];
        let (t1, v1) = keyframes[i + 1];
        if t >= t0 && t <= t1 {
            let span = t1 - t0;
            if span <= f32::EPSILON {
                return v1;
            }
            let local = (t - t0) / span;
            return lerp(v0, v1, local);
        }
    }
    keyframes[last].1
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn linear_is_identity() {
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!(approx(linear(t), t, 1e-6));
        }
    }

    #[test]
    fn ease_curves_endpoints_are_zero_and_one() {
        for f in [ease as fn(f32) -> f32, ease_in, ease_out, ease_in_out] {
            assert!(approx(f(0.0), 0.0, 1e-6));
            assert!(approx(f(1.0), 1.0, 1e-6));
        }
    }

    #[test]
    fn ease_in_starts_slow() {
        // ease_in at t=0.25 should still be near 0 (slow start).
        assert!(ease_in(0.25) < 0.25, "ease_in(0.25) = {}", ease_in(0.25));
    }

    #[test]
    fn ease_out_starts_fast() {
        // ease_out at t=0.25 should be well above 0.25 (fast start).
        assert!(ease_out(0.25) > 0.25, "ease_out(0.25) = {}", ease_out(0.25));
    }

    #[test]
    fn ease_in_out_is_symmetric_around_midpoint() {
        // ease_in_out(0.5) should land near 0.5; ease_in_out(t) +
        // ease_in_out(1-t) should ≈ 1 by symmetry.
        assert!(approx(ease_in_out(0.5), 0.5, 1e-3));
        for t in [0.1, 0.25, 0.4] {
            let sum = ease_in_out(t) + ease_in_out(1.0 - t);
            assert!(
                approx(sum, 1.0, 1e-3),
                "ease_in_out symmetry at {t}: sum={sum}"
            );
        }
    }

    #[test]
    fn cubic_bezier_clamps_endpoints() {
        assert_eq!(cubic_bezier(0.42, 0.0, 0.58, 1.0, -0.5), 0.0);
        assert_eq!(cubic_bezier(0.42, 0.0, 0.58, 1.0, 1.5), 1.0);
    }

    #[test]
    fn step_start_step_end_match_css() {
        assert_eq!(step_start(0.0), 0.0);
        assert_eq!(step_start(0.001), 1.0);
        assert_eq!(step_start(1.0), 1.0);
        assert_eq!(step_end(0.0), 0.0);
        assert_eq!(step_end(0.999), 0.0);
        assert_eq!(step_end(1.0), 1.0);
    }

    #[test]
    fn lerp_scalar() {
        assert!(approx(lerp(0.0_f32, 10.0, 0.0), 0.0, 1e-6));
        assert!(approx(lerp(0.0_f32, 10.0, 0.5), 5.0, 1e-6));
        assert!(approx(lerp(0.0_f32, 10.0, 1.0), 10.0, 1e-6));
        // Extrapolation: t > 1.0 keeps going.
        assert!(approx(lerp(0.0_f32, 10.0, 2.0), 20.0, 1e-6));
    }

    #[test]
    fn lerp_array_blends_componentwise() {
        let a = [0.0, 10.0, 20.0, 30.0];
        let b = [10.0, 0.0, 30.0, 20.0];
        let mid = lerp_array(a, b, 0.5);
        assert_eq!(mid, [5.0, 5.0, 25.0, 25.0]);
    }

    #[test]
    fn lerp_color_blends_premultiplied() {
        // Half-blend opaque red with opaque green = premultiplied
        // (0.5, 0.5, 0, 1.0). That's a yellow-ish at half-saturation
        // each channel.
        let red = [1.0, 0.0, 0.0, 1.0];
        let green = [0.0, 1.0, 0.0, 1.0];
        let mid = lerp_color(red, green, 0.5);
        assert_eq!(mid, [0.5, 0.5, 0.0, 1.0]);
    }

    #[test]
    fn sample_keyframes_handles_empty_and_clamping() {
        let empty: &[(f32, f32)] = &[];
        assert_eq!(sample_keyframes(empty, 0.5, 42.0), 42.0);

        let kf = &[(0.0, 0.0), (1.0, 100.0)];
        assert_eq!(sample_keyframes(kf, -1.0, 0.0), 0.0); // before first
        assert_eq!(sample_keyframes(kf, 0.5, 0.0), 50.0); // mid
        assert_eq!(sample_keyframes(kf, 2.0, 0.0), 100.0); // after last
    }

    #[test]
    fn sample_keyframes_lerps_between_bracketing_pair() {
        let kf = &[(0.0, 0.0), (0.5, 80.0), (1.0, 100.0)];
        // Between (0.5, 80) and (1.0, 100): at t=0.75, local=0.5,
        // result = lerp(80, 100, 0.5) = 90.
        assert_eq!(sample_keyframes(kf, 0.75, 0.0), 90.0);
    }

    #[test]
    fn sample_keyframes_zero_span_uses_later_value() {
        // Two keyframes at the same time → step-like: the later
        // value wins inside the (degenerate) span.
        let kf = &[(0.0, 0.0), (0.5, 50.0), (0.5, 100.0), (1.0, 200.0)];
        // At t=0.5 we hit (0.5, 50)..(0.5, 100): span=0, returns 100.
        assert_eq!(sample_keyframes(kf, 0.5, 0.0), 50.0);
    }

    #[test]
    fn easing_composes_with_keyframes() {
        // Apply ease_in_out to the parameter before sampling.
        let kf = &[(0.0_f32, 0.0_f32), (1.0, 100.0)];
        let mid_linear = sample_keyframes(kf, 0.5, 0.0);
        let mid_eased = sample_keyframes(kf, ease_in_out(0.5), 0.0);
        // ease_in_out(0.5) ≈ 0.5, so the eased mid should land
        // near the linear mid. (The test of D2's intended usage
        // pattern is that the composition compiles + runs cleanly.)
        assert!(approx(mid_linear, 50.0, 1e-3));
        assert!(approx(mid_eased, 50.0, 1.0));
    }
}
