/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! CommandEncoder lifecycle; submit / present. See plan §6 S1.

/// Create the command encoder for one renderer frame or offscreen
/// receipt. Renderer-body migration routes encoder ownership through
/// this module instead of constructing it ad hoc from `core.device`.
pub fn create_encoder(device: &wgpu::Device, label: &str) -> wgpu::CommandEncoder {
    device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) })
}

/// Finish and submit one command encoder. Surface presentation will
/// layer on top of this once the renderer owns a wgpu surface target;
/// offscreen tests already use the same command lifecycle.
pub fn submit(queue: &wgpu::Queue, encoder: wgpu::CommandEncoder) {
    queue.submit([encoder.finish()]);
}
