/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `GpuResources` impl for `WgpuDevice` (P4b/c/d/e/g/h).
//!
//! Texture/buffer/VAO ownership and uploads. All methods are real impls
//! except three GAT-bound stubs (`upload_texture`, `map_pbo_for_readback`,
//! `create_custom_vao`) which need additional design work.

use api::{ImageBufferKind, ImageFormat};
use api::units::DeviceIntSize;
use std::cell::{Cell, RefCell};
use std::num::NonZeroUsize;

use super::super::traits::GpuResources;
use super::super::types::{
    Texel, TextureFilter, VertexDescriptor, VertexUsageHint,
};
use super::types::{
    image_format_to_wgpu, WgpuBoundPbo, WgpuCustomVao, WgpuDrawTarget,
    WgpuExternalTexture, WgpuPbo, WgpuReadTarget, WgpuRenderTargetHandle,
    WgpuStream, WgpuTexture, WgpuTextureUploader, WgpuUploadPboPool, WgpuVao,
    WgpuVbo,
};
use super::WgpuDevice;

/// Reinterprets a typed slice as bytes for `queue.write_buffer` /
/// `queue.write_texture`. Sound under the trait contract that V is
/// plain-old-data shaped.
pub(super) fn slice_to_bytes<V>(slice: &[V]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            slice.as_ptr() as *const u8,
            slice.len() * std::mem::size_of::<V>(),
        )
    }
}

/// Ensures the `RefCell<Option<wgpu::Buffer>>` slot holds a buffer with
/// at least `bytes_needed` capacity, then writes the given data starting
/// at offset 0. Allocates (or reallocates when growing) as needed; never
/// shrinks. Used by `update_vao_*`.
pub(super) fn upload_into_vao_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &RefCell<Option<wgpu::Buffer>>,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
    label: &'static str,
) {
    let needed = bytes.len() as u64;
    if needed == 0 {
        return;
    }
    let mut borrow = slot.borrow_mut();
    let needs_new = match borrow.as_ref() {
        Some(buf) => buf.size() < needed,
        None => true,
    };
    if needs_new {
        *borrow = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: needed,
            usage,
            mapped_at_creation: false,
        }));
    }
    if let Some(buf) = borrow.as_ref() {
        queue.write_buffer(buf, 0, bytes);
    }
}

impl GpuResources for WgpuDevice {
    type Texture = WgpuTexture;
    type Vao = WgpuVao;
    type CustomVao = WgpuCustomVao;
    type Pbo = WgpuPbo;
    type Stream<'a> = WgpuStream<'a>;
    type Vbo<T> = WgpuVbo<T>;
    type BoundPbo<'a> = WgpuBoundPbo<'a> where Self: 'a;
    type TextureUploader<'a> = WgpuTextureUploader<'a>;
    type RenderTargetHandle = WgpuRenderTargetHandle;
    type ReadTarget = WgpuReadTarget;
    type DrawTarget = WgpuDrawTarget;
    type ExternalTexture = WgpuExternalTexture;
    type UploadPboPool = WgpuUploadPboPool;

    fn create_texture(
        &mut self,
        target: ImageBufferKind,
        format: ImageFormat,
        width: i32,
        height: i32,
        filter: TextureFilter,
        render_target: Option<crate::internal_types::RenderTargetInfo>,
    ) -> Self::Texture {
        // Clamp to wgpu's max texture dimension (matches GL device's clamp).
        let max_dim = self.device().limits().max_texture_dimension_2d as i32;
        let w = width.min(max_dim).max(1) as u32;
        let h = height.min(max_dim).max(1) as u32;

        let is_render_target = render_target.is_some();
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST;
        if is_render_target {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }

        let wgpu_format = image_format_to_wgpu(format);
        let texture = self.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("WgpuDevice::create_texture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            // ImageBufferKind::TextureExternal/BT709 also resolve to D2 in
            // wgpu; external interop happens via a separate path.
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("WgpuDevice::create_texture default view"),
            ..Default::default()
        });

        WgpuTexture {
            texture,
            view,
            format,
            size: api::units::DeviceIntSize::new(w as i32, h as i32),
            filter,
            target,
            is_render_target,
        }
    }

    fn delete_texture(&mut self, _texture: Self::Texture) {
        // wgpu::Texture is Drop-managed; falling out of scope releases
        // the GPU resource (deferred until in-flight command buffers
        // using it complete).
    }

    fn copy_entire_texture(&mut self, dst: &mut Self::Texture, src: &Self::Texture) {
        // One-shot encoder per call — simple but inefficient. Batching
        // into a per-frame encoder is a P5+ optimization.
        let copy_w = src.size.width.min(dst.size.width).max(0) as u32;
        let copy_h = src.size.height.min(dst.size.height).max(0) as u32;
        if copy_w == 0 || copy_h == 0 {
            return;
        }
        let mut encoder = self.device().create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("WgpuDevice::copy_entire_texture"),
            },
        );
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dst.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: copy_w, height: copy_h, depth_or_array_layers: 1 },
        );
        self.queue().submit([encoder.finish()]);
    }

    fn copy_texture_sub_region(
        &mut self,
        src_texture: &Self::Texture,
        src_x: usize,
        src_y: usize,
        dest_texture: &Self::Texture,
        dest_x: usize,
        dest_y: usize,
        width: usize,
        height: usize,
    ) {
        let mut encoder = self.device().create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("WgpuDevice::copy_texture_sub_region"),
            },
        );
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: src_x as u32, y: src_y as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dest_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: dest_x as u32, y: dest_y as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
        );
        self.queue().submit([encoder.finish()]);
    }

    fn invalidate_render_target(&mut self, _texture: &Self::Texture) {
        // GL-only optimization hint (`glInvalidateFramebuffer`). wgpu's
        // render pass `LoadOp::Clear`/`LoadOp::DontCare` semantics handle
        // the same intent at pass-start time; no per-texture action.
    }

    fn invalidate_depth_target(&mut self) {
        // Same as invalidate_render_target — no-op in wgpu's model.
    }

    fn reuse_render_target<T: Texel>(
        &mut self,
        _texture: &mut Self::Texture,
        _rt_info: crate::internal_types::RenderTargetInfo,
    ) {
        // GL: updates last-frame-used cache metadata + lazily attaches
        // a depth render-buffer when needed. wgpu textures are immutable
        // in shape post-creation; depth-attach mid-life would require
        // recreation. No-op for now; depth-bearing render targets must
        // be created with the right attachment usage from `create_texture`.
        // Revisit when the renderer's render-target-cache machinery
        // wires through this trait.
    }

    fn create_fbo(&mut self) -> Self::RenderTargetHandle {
        // wgpu has no FBO concept — render passes attach a TextureView
        // directly when started. Marker handle today; the
        // texture-view-flowing-via-DrawTarget approach (Option II of the
        // FBO design discussion) lands when P5 actually drives draws.
        WgpuRenderTargetHandle
    }

    fn create_fbo_for_external_texture(&mut self, _texture_id: u32) -> Self::RenderTargetHandle {
        // Same as create_fbo. The texture_id parameter is a raw GLuint
        // intended for the GL device; wgpu external-texture interop
        // happens through a separate path.
        WgpuRenderTargetHandle
    }

    fn delete_fbo(&mut self, _fbo: Self::RenderTargetHandle) {
        // Marker-only; nothing to release.
    }

    fn create_pbo(&mut self) -> Self::Pbo {
        WgpuPbo { buffer: None, size: 0 }
    }

    fn create_pbo_with_size(&mut self, size: usize) -> Self::Pbo {
        // wgpu enforces "MAP_* combines only with the opposite COPY_*".
        // Sticking to readback orientation (texture/buffer -> CPU) which
        // is what map_pbo_for_readback drives. Upload PBOs in wgpu are
        // typically replaced by queue.write_buffer/write_texture.
        let buffer = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("WgpuPbo"),
            size: size as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        WgpuPbo { buffer: Some(buffer), size }
    }

    fn delete_pbo(&mut self, _pbo: Self::Pbo) {
        // wgpu::Buffer is Drop-managed.
    }

    fn create_vao(&mut self, descriptor: &VertexDescriptor, instance_divisor: u32) -> Self::Vao {
        // Buffers are lazy: actual wgpu::Buffer creation happens on first
        // update_vao_* call. Matches GL device's behavior; avoids
        // speculative allocation.
        WgpuVao {
            vertex_buffer: RefCell::new(None),
            vertex_count: Cell::new(0),
            instance_buffer: RefCell::new(None),
            instance_count: Cell::new(0),
            index_buffer: RefCell::new(None),
            index_count: Cell::new(0),
            descriptor: VertexDescriptor {
                vertex_attributes: descriptor.vertex_attributes,
                instance_attributes: descriptor.instance_attributes,
            },
            instance_divisor,
        }
    }

    fn create_vao_with_new_instances(
        &mut self,
        descriptor: &VertexDescriptor,
        base_vao: &Self::Vao,
    ) -> Self::Vao {
        // GL: shares vertex + index buffers with base_vao + fresh instance
        // buffer. wgpu::Buffer isn't Clone, so we can't share by handle
        // directly; renderer must repopulate via update_vao_* if needed.
        // (Future opt: Arc-wrapped buffers for cheap sharing.)
        WgpuVao {
            vertex_buffer: RefCell::new(None),
            vertex_count: Cell::new(base_vao.vertex_count.get()),
            instance_buffer: RefCell::new(None),
            instance_count: Cell::new(0),
            index_buffer: RefCell::new(None),
            index_count: Cell::new(base_vao.index_count.get()),
            descriptor: VertexDescriptor {
                vertex_attributes: descriptor.vertex_attributes,
                instance_attributes: descriptor.instance_attributes,
            },
            instance_divisor: base_vao.instance_divisor,
        }
    }

    fn delete_vao(&mut self, _vao: Self::Vao) {
        // Drop releases the wgpu::Buffer handles inside.
    }

    fn create_custom_vao<'a>(&mut self, _streams: &[Self::Stream<'a>]) -> Self::CustomVao {
        // wgpu's Self::Stream<'a> is the placeholder WgpuStream<'a> (no
        // constructors). Renderer code that wants a custom multi-stream
        // VAO needs to construct WgpuStream values first, which means
        // this method is effectively unreachable through cross-backend
        // code paths today. Real impl lands when a renderer call site
        // actually wires through.
        unimplemented!("create_custom_vao on wgpu requires a WgpuStream constructor (deferred)")
    }

    fn delete_custom_vao(&mut self, _vao: Self::CustomVao) {
        // Drop releases the buffers.
    }

    fn create_vbo<T>(&mut self) -> Self::Vbo<T> {
        WgpuVbo::new()
    }

    fn delete_vbo<T>(&mut self, _vbo: Self::Vbo<T>) {
        // Drop releases the wgpu::Buffer.
    }

    fn allocate_vbo<V>(
        &mut self,
        vbo: &mut Self::Vbo<V>,
        count: usize,
        _usage_hint: VertexUsageHint,
    ) {
        // wgpu buffers are immutable in size — recreate when size changes.
        // Broad usage flags (VERTEX | INDEX | COPY_*) so the same Vbo can
        // serve as either vertex or index buffer; renderer picks at draw
        // time. Refining usage from `_usage_hint` is a future opt
        // (Static vs Dynamic might inform memory hints).
        let size = (count * std::mem::size_of::<V>()) as u64;
        let buffer = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("WgpuVbo"),
            size,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        vbo.buffer = Some(buffer);
        vbo.count = count;
    }

    fn fill_vbo<V>(&mut self, vbo: &Self::Vbo<V>, data: &[V], offset: usize) {
        let buf = vbo
            .buffer
            .as_ref()
            .expect("fill_vbo before allocate_vbo");
        let bytes = slice_to_bytes(data);
        let byte_offset = (offset * std::mem::size_of::<V>()) as u64;
        self.queue().write_buffer(buf, byte_offset, bytes);
    }

    fn update_vao_main_vertices<V>(
        &mut self,
        vao: &Self::Vao,
        vertices: &[V],
        _usage_hint: VertexUsageHint,
    ) {
        upload_into_vao_buffer(
            self.device(),
            self.queue(),
            &vao.vertex_buffer,
            slice_to_bytes(vertices),
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            "WgpuVao vertex_buffer",
        );
        vao.vertex_count.set(vertices.len());
    }

    fn update_vao_instances<V: Clone>(
        &mut self,
        vao: &Self::Vao,
        instances: &[V],
        _usage_hint: VertexUsageHint,
        repeat: Option<NonZeroUsize>,
    ) {
        // GL's `repeat` parameter writes the instance data N times for
        // workarounds where instance attribute divisors aren't reliable.
        // Honor by expanding before upload when N > 1.
        let multiplier = repeat.map(|n| n.get()).unwrap_or(1);
        let total_count = instances.len() * multiplier;
        if multiplier == 1 {
            upload_into_vao_buffer(
                self.device(),
                self.queue(),
                &vao.instance_buffer,
                slice_to_bytes(instances),
                wgpu::BufferUsages::VERTEX
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                "WgpuVao instance_buffer",
            );
        } else {
            let mut expanded: Vec<V> = Vec::with_capacity(total_count);
            for v in instances {
                for _ in 0..multiplier {
                    expanded.push(v.clone());
                }
            }
            upload_into_vao_buffer(
                self.device(),
                self.queue(),
                &vao.instance_buffer,
                slice_to_bytes(&expanded),
                wgpu::BufferUsages::VERTEX
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                "WgpuVao instance_buffer",
            );
        }
        vao.instance_count.set(total_count);
    }

    fn update_vao_indices<I>(
        &mut self,
        vao: &Self::Vao,
        indices: &[I],
        _usage_hint: VertexUsageHint,
    ) {
        upload_into_vao_buffer(
            self.device(),
            self.queue(),
            &vao.index_buffer,
            slice_to_bytes(indices),
            wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            "WgpuVao index_buffer",
        );
        vao.index_count.set(indices.len());
    }

    fn upload_texture<'a>(&mut self, _pbo_pool: &'a mut Self::UploadPboPool) -> Self::TextureUploader<'a> {
        // GAT-bound stub — needs upload-batching design. queue.write_texture
        // already batches internally so this wrapper may end up doing
        // nothing useful for wgpu beyond satisfying the trait. Defer until
        // renderer actually exercises this path.
        unimplemented!("upload_texture on wgpu — design deferred")
    }

    fn upload_texture_immediate<T: Texel>(&mut self, texture: &Self::Texture, pixels: &[T]) {
        // Convert typed pixel slice to bytes. Texel guarantees Copy + Default;
        // by trait contract, T's size matches texture.format.bytes_per_pixel.
        let pixels_bytes = slice_to_bytes(pixels);

        let width = texture.size.width.max(0) as u32;
        let height = texture.size.height.max(0) as u32;
        let bytes_per_pixel = texture.format.bytes_per_pixel() as u32;
        let bytes_per_row = bytes_per_pixel * width;

        // queue.write_texture handles copy-on-submit (next queue submission
        // includes this upload); no explicit encoder needed.
        self.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    fn map_pbo_for_readback<'a>(&'a mut self, _pbo: &'a Self::Pbo) -> Option<Self::BoundPbo<'a>> {
        // GAT-bound stub — needs async-map handling. wgpu's map is async
        // and requires polling; the synchronous GL pattern doesn't fit
        // without blocking. Defer until renderer drives this path.
        unimplemented!("map_pbo_for_readback on wgpu — async-map design deferred")
    }

    fn attach_read_texture(&mut self, _texture: &Self::Texture) {
        // GL: binds a texture as the current GL_READ_FRAMEBUFFER attachment
        // for subsequent glReadPixels. wgpu reads happen via
        // copy_texture_to_buffer + buffer mapping (see existing
        // texture-readback test pattern); no long-lived "attached read
        // texture" state. Renderer's calls here are honored implicitly
        // by passing the right texture to its readback paths (P5+).
    }

    fn required_upload_size_and_stride(
        &self,
        size: DeviceIntSize,
        format: ImageFormat,
    ) -> (usize, usize) {
        // Aligned-row pitch for buffer-to-texture copies: 256-byte aligned
        // per wgpu's COPY_BYTES_PER_ROW_ALIGNMENT.
        let bytes_per_pixel = format.bytes_per_pixel() as usize;
        let unaligned = (size.width.max(0) as usize) * bytes_per_pixel;
        let stride = (unaligned + 255) & !255;
        let total = stride * (size.height.max(0) as usize);
        (total, stride)
    }
}
