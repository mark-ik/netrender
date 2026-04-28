/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! wgpu device. Decomposed per the idiomatic-wgsl pipeline plan §6 S1
//! (no file > ~600 LOC). See
//! `wr-wgpu-notes/2026-04-28_idiomatic_wgsl_pipeline_plan.md`.

pub mod core;
pub mod format;
pub mod buffer;
pub mod texture;
pub mod shader;
pub mod binding;
pub mod pipeline;
pub mod pass;
pub mod frame;
pub mod readback;
pub mod adapter;

#[cfg(test)]
mod tests;
