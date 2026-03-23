/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! wgpu backend device — Stage 4a.
//!
//! Grows `WgpuDevice` from a stub into a headless-capable prototype that can:
//!   - initialise a real `wgpu::Device` and `wgpu::Queue` (no surface required)
//!   - manage frame lifecycle (`begin_frame` / `end_frame`)
//!   - create, upload, and delete RGBA/R/RG textures
//!
//! Draw calls and pixel readback are not yet implemented and will
//! `unimplemented!()`.  They are addressed in Stage 4b once render
//! pipelines and WGSL shaders are wired up.

use api::{ImageBufferKind, ImageFormat};
use api::units::FramebufferIntRect;
use crate::device::{GpuDevice, GpuFrameId, RenderTargetInfo, TextureFilter};

// ── Opaque resource types ─────────────────────────────────────────────────────

/// A wgpu-backed texture handle.
pub struct WgpuTexture {
    texture: wgpu::Texture,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
}

/// A wgpu-backed shader pipeline.  Stub until Stage 4b adds render pipelines.
pub struct WgpuProgram;

// ── WgpuDevice ────────────────────────────────────────────────────────────────

/// A WebGPU-backed rendering device.
pub struct WgpuDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Bitset of features actually enabled on this device.
    features: wgpu::Features,
    frame_id: usize,
}

impl WgpuDevice {
    /// Construct a headless `WgpuDevice` with no surface, suitable for
    /// off-screen rendering and unit tests.
    ///
    /// Returns `None` if no wgpu-capable adapter is available (e.g., a
    /// headless CI machine without Vulkan/lavapipe).
    pub fn new_headless() -> Option<Self> {
        pollster::block_on(async {
            let instance = wgpu::Instance::default();

            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::None,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .ok()?;

            // Request 16-bit normalised texture formats if the adapter
            // supports them.  These are needed for ImageFormat::R16 / RG16.
            let wanted = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
            let required_features = adapter.features() & wanted;

            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("WebRender wgpu device"),
                        required_features,
                        ..Default::default()
                    },
                )
                .await
                .ok()?;

            Some(WgpuDevice {
                device,
                queue,
                features: required_features,
                frame_id: 0,
            })
        })
    }
}

// ── WgpuDevice render-pass helpers ────────────────────────────────────────────

impl WgpuDevice {
    /// Clear a render-target texture to a solid RGBA colour.
    ///
    /// This is wgpu-specific (not part of `GpuDevice`) and is the first real
    /// encoder / render-pass operation in the backend.  It proves that command
    /// encoding and queue submission work end-to-end before draw calls land in
    /// Stage 4c.
    pub fn clear_texture(&self, texture: &WgpuTexture, color: [f64; 4]) {
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("clear_texture") },
        );
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: color[0],
                            g: color[1],
                            b: color[2],
                            a: color[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit([encoder.finish()]);
    }
}

// ── GpuDevice implementation ──────────────────────────────────────────────────

impl GpuDevice for WgpuDevice {
    type Texture = WgpuTexture;
    type Program = WgpuProgram;

    // --- Frame lifecycle ---

    fn begin_frame(&mut self) -> GpuFrameId {
        self.frame_id = self.frame_id.wrapping_add(1);
        GpuFrameId::new(self.frame_id)
    }

    fn end_frame(&mut self) {
        // Submit any outstanding work accumulated during the frame.  At this
        // stage there is nothing to submit; real command buffers arrive in
        // Stage 4b.
        self.queue.submit(std::iter::empty());
    }

    // --- Texture management ---

    fn create_texture(
        &mut self,
        target: ImageBufferKind,
        format: ImageFormat,
        width: i32,
        height: i32,
        _filter: TextureFilter,
        render_target: Option<RenderTargetInfo>,
    ) -> Self::Texture {
        let wgpu_format = image_format_to_wgpu(format, self.features);
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        if render_target.is_some() {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: image_buffer_kind_to_texture_dimension(target),
            format: wgpu_format,
            usage,
            view_formats: &[],
        });
        WgpuTexture { texture, format: wgpu_format, width: width as u32, height: height as u32 }
    }

    fn upload_texture_immediate(&mut self, texture: &Self::Texture, pixels: &[u8]) {
        let bytes_per_row = texture.width * wgpu_format_bytes_per_pixel(texture.format);
        self.queue.write_texture(
            texture.texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: texture.width,
                height: texture.height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn delete_texture(&mut self, texture: Self::Texture) {
        drop(texture); // wgpu::Texture cleanup is handled by Drop.
    }

    // --- Draw calls (Stage 4b) ---

    fn draw_triangles_u16(&mut self, _first_vertex: i32, _index_count: i32) {
        unimplemented!("draw_triangles_u16: requires render pass — Stage 4b");
    }

    fn draw_triangles_u32(&mut self, _first_vertex: i32, _index_count: i32) {
        unimplemented!("draw_triangles_u32: requires render pass — Stage 4b");
    }

    // --- Readback (Stage 4b) ---

    fn read_pixels_into(
        &mut self,
        _rect: FramebufferIntRect,
        _format: ImageFormat,
        _output: &mut [u8],
    ) {
        unimplemented!("read_pixels_into: async readback not yet implemented — Stage 4b");
    }
}

// ── Format helpers ────────────────────────────────────────────────────────────

fn image_format_to_wgpu(format: ImageFormat, features: wgpu::Features) -> wgpu::TextureFormat {
    match format {
        ImageFormat::R8 => wgpu::TextureFormat::R8Unorm,
        ImageFormat::BGRA8 => wgpu::TextureFormat::Bgra8Unorm,
        ImageFormat::RGBA8 => wgpu::TextureFormat::Rgba8Unorm,
        ImageFormat::RG8 => wgpu::TextureFormat::Rg8Unorm,
        ImageFormat::RGBAF32 => wgpu::TextureFormat::Rgba32Float,
        // R16 / RG16 require TEXTURE_FORMAT_16BIT_NORM.  The device requests
        // this feature when present; if absent these formats are unavailable.
        ImageFormat::R16 => {
            assert!(
                features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
                "ImageFormat::R16 requires wgpu::Features::TEXTURE_FORMAT_16BIT_NORM"
            );
            wgpu::TextureFormat::R16Unorm
        }
        ImageFormat::RG16 => {
            assert!(
                features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
                "ImageFormat::RG16 requires wgpu::Features::TEXTURE_FORMAT_16BIT_NORM"
            );
            wgpu::TextureFormat::Rg16Unorm
        }
        // RGBAI32 is a signed 32-bit integer RGBA format.
        ImageFormat::RGBAI32 => wgpu::TextureFormat::Rgba32Sint,
    }
}

fn image_buffer_kind_to_texture_dimension(kind: ImageBufferKind) -> wgpu::TextureDimension {
    match kind {
        // wgpu has no rectangle texture type.  TextureRect is treated as D2;
        // the non-power-of-two / no-mipmaps semantics are preserved by the
        // texture descriptor (mip_level_count = 1).
        ImageBufferKind::Texture2D
        | ImageBufferKind::TextureRect
        | ImageBufferKind::TextureExternal
        | ImageBufferKind::TextureExternalBT709 => wgpu::TextureDimension::D2,
    }
}

/// Returns the number of bytes per texel for a supported `wgpu::TextureFormat`.
fn wgpu_format_bytes_per_pixel(format: wgpu::TextureFormat) -> u32 {
    match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm => 4,
        wgpu::TextureFormat::R8Unorm => 1,
        wgpu::TextureFormat::R16Unorm => 2,
        wgpu::TextureFormat::Rg8Unorm => 2,
        wgpu::TextureFormat::Rg16Unorm => 4,
        wgpu::TextureFormat::Rgba32Float | wgpu::TextureFormat::Rgba32Sint => 16,
        f => panic!("wgpu_format_bytes_per_pixel: unhandled format {:?}", f),
    }
}

// ── Headless unit tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use api::{ImageBufferKind, ImageFormat};
    use crate::device::{RenderTargetInfo, TextureFilter};

    /// Attempt to create a headless device.  Returns `None` and prints a
    /// notice if no adapter is available (e.g., in server CI without a GPU or
    /// Vulkan software renderer).
    fn try_device() -> Option<WgpuDevice> {
        let dev = WgpuDevice::new_headless();
        if dev.is_none() {
            eprintln!("wgpu: no adapter available — skipping test");
        }
        dev
    }

    #[test]
    fn headless_init_and_frame_lifecycle() {
        let Some(mut dev) = try_device() else { return };
        let _frame_id = dev.begin_frame();
        dev.end_frame();
    }

    #[test]
    fn texture_create_and_upload() {
        let Some(mut dev) = try_device() else { return };
        let tex = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            4, 4,
            TextureFilter::Linear,
            None,
        );
        let pixels = vec![0xffu8; 4 * 4 * 4];
        dev.upload_texture_immediate(&tex, &pixels);
        dev.delete_texture(tex);
    }

    #[test]
    fn render_target_texture() {
        let Some(mut dev) = try_device() else { return };
        let tex = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            32, 32,
            TextureFilter::Linear,
            Some(RenderTargetInfo { has_depth: false }),
        );
        dev.delete_texture(tex);
    }

    #[test]
    fn clear_render_target() {
        // Exercises the first real render-pass: encoder creation, clear load-op,
        // and queue submission.  Confirms the GPU command infrastructure works
        // before draw calls land in Stage 4c.
        let Some(mut dev) = try_device() else { return };
        let tex = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            32, 32,
            TextureFilter::Nearest,
            Some(RenderTargetInfo { has_depth: false }),
        );
        // Clear to opaque red.  No assertion on pixel values — readback is
        // added in Stage 4c alongside the first draw pass.
        dev.clear_texture(&tex, [1.0, 0.0, 0.0, 1.0]);
        dev.delete_texture(tex);
    }
}
