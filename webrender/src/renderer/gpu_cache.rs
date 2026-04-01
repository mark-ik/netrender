/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use std::{cmp, mem};
use api::units::*;
use malloc_size_of::MallocSizeOfOps;
use crate::{
    device::{CustomVAO, Device, DrawTarget, GpuDevice, Program, ReadTarget, Texture, TextureFilter, UploadPBOPool, VBO},
    gpu_cache::{GpuBlockData, GpuCacheUpdate, GpuCacheUpdateList},
    internal_types::{FrameId, RenderTargetInfo, Swizzle},
    prim_store::DeferredResolve,
    profiler,
    render_api::MemoryReport,
};

trait GpuCacheStorageDevice: GpuDevice<Texture = Texture> {
    fn supports_copy_image_sub_data(&self) -> bool;
    fn supports_color_buffer_float(&self) -> bool;
    fn copy_entire_texture(&mut self, dst: &mut Texture, src: &Texture);
}

impl GpuCacheStorageDevice for Device {
    fn supports_copy_image_sub_data(&self) -> bool {
        self.get_capabilities().supports_copy_image_sub_data
    }

    fn supports_color_buffer_float(&self) -> bool {
        self.get_capabilities().supports_color_buffer_float
    }

    fn copy_entire_texture(&mut self, dst: &mut Texture, src: &Texture) {
        Device::copy_entire_texture(self, dst, src);
    }
}

/// Enabling this toggle would force the GPU cache scattered texture to
/// be resized every frame, which enables GPU debuggers to see if this
/// is performed correctly.
const GPU_CACHE_RESIZE_TEST: bool = false;

use super::gpu_cache_utils::{CacheRow, for_each_gpu_cache_copy};

/// Backend-specific GPU cache update machinery.
enum GpuCacheBackend {
    /// PBO-based updates, currently operate on a row granularity.
    /// Therefore, are subject to fragmentation issues.
    PixelBuffer {
        /// Per-row data.
        rows: Vec<CacheRow>,
    },
    /// Shader-based scattering updates. Currently rendered by a set
    /// of points into the GPU texture, each carrying a `GpuBlockData`.
    Scatter {
        /// Special program to run the scattered update.
        program: Program,
        /// VAO containing the source vertex buffers.
        vao: CustomVAO,
        /// VBO for positional data, supplied as normalized `u16`.
        buf_position: VBO<[u16; 2]>,
        /// VBO for gpu block data.
        buf_value: VBO<GpuBlockData>,
        /// Currently stored block count.
        count: usize,
    },
}

/// Shared texture storage for the GPU cache, independent of update backend.
struct GpuCacheTextureStorage {
    texture: Option<Texture>,
}

impl GpuCacheTextureStorage {
    /// Ensures that we have an appropriately-sized texture.
    fn ensure_texture<D: GpuCacheStorageDevice>(
        &mut self,
        backend: &mut GpuCacheBackend,
        device: &mut D,
        height: i32,
    ) {
        // If we already have a texture that works, we're done.
        if self.texture.as_ref().map_or(false, |t| t.get_dimensions().height >= height) {
            if GPU_CACHE_RESIZE_TEST {
                // Special debug mode - resize the texture even though it's fine.
            } else {
                return;
            }
        }

        // Take the old texture, if any.
        let blit_source = self.texture.take();

        // Create the new texture.
        assert!(height >= 2, "Height is too small for ANGLE");
        let new_size = DeviceIntSize::new(super::MAX_VERTEX_TEXTURE_WIDTH as _, height);
        // GpuCacheBus::Scatter always requires the texture to be a render target. For
        // GpuCacheBus::PixelBuffer, we only create the texture with a render target if
        // RGBAF32 render targets are actually supported, and only if glCopyImageSubData
        // is not. glCopyImageSubData does not require a render target to copy the texture
        // data, and if neither RGBAF32 render targets nor glCopyImageSubData is supported,
        // we simply re-upload the entire contents rather than copying upon resize.
        let supports_copy_image_sub_data = device.supports_copy_image_sub_data();
        let supports_color_buffer_float = device.supports_color_buffer_float();
        let rt_info = if matches!(backend, GpuCacheBackend::PixelBuffer { .. })
            && (supports_copy_image_sub_data || !supports_color_buffer_float)
        {
            None
        } else {
            Some(RenderTargetInfo { has_depth: false })
        };
        let mut texture = device.create_texture(
            api::ImageBufferKind::Texture2D,
            api::ImageFormat::RGBAF32,
            new_size.width,
            new_size.height,
            TextureFilter::Nearest,
            rt_info,
        );

        // Copy the contents of the previous texture, if applicable.
        if let Some(blit_source) = blit_source {
            if !supports_copy_image_sub_data && !supports_color_buffer_float {
                // Cannot copy texture, so must re-upload everything.
                match backend {
                    GpuCacheBackend::PixelBuffer { ref mut rows } => {
                        for row in rows {
                            row.add_dirty(0, super::MAX_VERTEX_TEXTURE_WIDTH);
                        }
                    }
                    GpuCacheBackend::Scatter { .. } => {
                        panic!("Texture must be copyable to use scatter GPU cache bus method");
                    }
                }
            } else {
                device.copy_entire_texture(&mut texture, &blit_source);
            }
            device.delete_texture(blit_source);
        }

        self.texture = Some(texture);
    }

    fn take_texture(&mut self) -> Option<Texture> {
        self.texture.take()
    }

    fn get_height(&self) -> i32 {
        self.texture.as_ref().map_or(0, |t| t.get_dimensions().height)
    }

    fn report_memory_to(&self, report: &mut MemoryReport) {
        report.gpu_cache_textures += self.texture.as_ref().map_or(0, |t| t.size_in_bytes());
    }

    fn gpu_size_in_bytes(&self) -> usize {
        match &self.texture {
            Some(tex) => tex.size_in_bytes(),
            None => 0,
        }
    }
}

/// The device-specific representation of the GPU cache in gpu_cache.rs
pub struct GpuCacheTexture {
    storage: GpuCacheTextureStorage,
    backend: GpuCacheBackend,
}

#[cfg(feature = "wgpu_backend")]
#[allow(dead_code)]
pub struct WgpuGpuCacheTexture;

#[cfg_attr(feature = "wgpu_backend", allow(dead_code))]
pub enum RendererGpuCache {
    Gl(GpuCacheTexture),
    #[cfg(feature = "wgpu_backend")]
    #[allow(dead_code)]
    Wgpu(WgpuGpuCacheTexture),
}

impl GpuCacheTexture {

    pub fn new(device: &mut Device, use_scatter: bool) -> Result<Self, super::RendererError> {
        use super::desc::GPU_CACHE_UPDATE;

        let bus = if use_scatter {
            assert!(
                device.get_capabilities().supports_color_buffer_float,
                "GpuCache scatter method requires EXT_color_buffer_float",
            );
            let program = device.create_program_linked(
                "gpu_cache_update",
                &[],
                &GPU_CACHE_UPDATE,
            )?;
            let buf_position = device.create_vbo();
            let buf_value = device.create_vbo();
            //Note: the vertex attributes have to be supplied in the same order
            // as for program creation, but each assigned to a different stream.
            let vao = device.create_custom_vao(&[
                buf_position.stream_with(&GPU_CACHE_UPDATE.vertex_attributes[0..1]),
                buf_value   .stream_with(&GPU_CACHE_UPDATE.vertex_attributes[1..2]),
            ]);
            GpuCacheBackend::Scatter {
                program,
                vao,
                buf_position,
                buf_value,
                count: 0,
            }
        } else {
            GpuCacheBackend::PixelBuffer {
                rows: Vec::new(),
            }
        };

        Ok(GpuCacheTexture {
            storage: GpuCacheTextureStorage { texture: None },
            backend: bus,
        })
    }

    pub fn deinit(mut self, device: &mut Device) {
        if let Some(t) = self.storage.take_texture() {
            device.delete_texture(t);
        }
        if let GpuCacheBackend::Scatter { program, vao, buf_position, buf_value, .. } = self.backend {
            device.delete_program(program);
            device.delete_custom_vao(vao);
            device.delete_vbo(buf_position);
            device.delete_vbo(buf_value);
        }
    }

    pub fn get_height(&self) -> i32 {
        self.storage.get_height()
    }

    #[cfg(feature = "capture")]
    pub fn get_texture(&self) -> &Texture {
        self.storage.texture.as_ref().unwrap()
    }

    fn prepare_for_updates(
        &mut self,
        device: &mut Device,
        total_block_count: usize,
        max_height: i32,
    ) {
        self.storage.ensure_texture(&mut self.backend, device, max_height);
        match self.backend {
            GpuCacheBackend::PixelBuffer { .. } => {},
            GpuCacheBackend::Scatter {
                ref mut buf_position,
                ref mut buf_value,
                ref mut count,
                ..
            } => {
                *count = 0;
                if total_block_count > buf_value.allocated_count() {
                    device.allocate_vbo(buf_position, total_block_count, super::ONE_TIME_USAGE_HINT);
                    device.allocate_vbo(buf_value,    total_block_count, super::ONE_TIME_USAGE_HINT);
                }
            }
        }
    }

    pub fn invalidate(&mut self) {
        match self.backend {
            GpuCacheBackend::PixelBuffer { ref mut rows, .. } => {
                info!("Invalidating GPU caches");
                for row in rows {
                    row.add_dirty(0, super::MAX_VERTEX_TEXTURE_WIDTH);
                }
            }
            GpuCacheBackend::Scatter { .. } => {
                warn!("Unable to invalidate scattered GPU cache");
            }
        }
    }

    fn update(&mut self, device: &mut Device, updates: &GpuCacheUpdateList) {
        match self.backend {
            GpuCacheBackend::PixelBuffer { ref mut rows, .. } => {
                for_each_gpu_cache_copy(updates, |row_idx, col, blocks| {
                    // Ensure enough rows for this patch.
                    while rows.len() <= row_idx {
                        rows.push(CacheRow::new());
                    }
                    // Copy blocks into the CPU-side shadow.
                    rows[row_idx].cpu_blocks[col .. col + blocks.len()]
                        .copy_from_slice(blocks);
                    // Mark dirty for batched upload.
                    rows[row_idx].add_dirty(col, blocks.len());
                });
            }
            GpuCacheBackend::Scatter {
                ref buf_position,
                ref buf_value,
                ref mut count,
                ..
            } => {
                //TODO: re-use this heap allocation
                // Unused positions will be left as 0xFFFF, which translates to
                // (1.0, 1.0) in the vertex output position and gets culled out
                let mut position_data = vec![[!0u16; 2]; updates.blocks.len()];
                let size = self.storage.texture.as_ref().unwrap().get_dimensions().to_usize();

                for update in &updates.updates {
                    match *update {
                        GpuCacheUpdate::Copy {
                            block_index,
                            block_count,
                            address,
                        } => {
                            // Convert the absolute texel position into normalized
                            let y = ((2*address.v as usize + 1) << 15) / size.height;
                            for i in 0 .. block_count {
                                let x = ((2*address.u as usize + 2*i + 1) << 15) / size.width;
                                position_data[block_index + i] = [x as _, y as _];
                            }
                        }
                    }
                }

                device.fill_vbo(buf_value, &updates.blocks, *count);
                device.fill_vbo(buf_position, &position_data, *count);
                *count += position_data.len();
            }
        }
    }

    fn flush(&mut self, device: &mut Device, pbo_pool: &mut UploadPBOPool) -> usize {
        let texture = self.storage.texture.as_ref().unwrap();
        match self.backend {
            GpuCacheBackend::PixelBuffer { ref mut rows } => {
                let rows_dirty = rows
                    .iter()
                    .filter(|row| row.is_dirty())
                    .count();
                if rows_dirty == 0 {
                    return 0
                }

                let mut uploader = device.upload_texture(pbo_pool);

                for (row_index, row) in rows.iter_mut().enumerate() {
                    if !row.is_dirty() {
                        continue;
                    }

                    let blocks = row.dirty_blocks();
                    let rect = DeviceIntRect::from_origin_and_size(
                        DeviceIntPoint::new(row.min_dirty() as i32, row_index as i32),
                        DeviceIntSize::new(blocks.len() as i32, 1),
                    );

                    uploader.upload(device, texture, rect, None, None, blocks.as_ptr(), blocks.len());

                    row.clear_dirty();
                }

                uploader.flush(device);

                rows_dirty
            }
            GpuCacheBackend::Scatter { ref program, ref vao, count, .. } => {
                device.disable_depth();
                device.set_blend(false);
                device.bind_program(program);
                device.bind_custom_vao(vao);
                device.bind_draw_target(
                    DrawTarget::from_texture(
                        texture,
                        false,
                    ),
                );
                device.draw_nonindexed_points(0, count as _);
                0
            }
        }
    }

    #[cfg(feature = "replay")]
    pub fn remove_texture(&mut self, device: &mut Device) {
        if let Some(t) = self.storage.take_texture() {
            device.delete_texture(t);
        }
    }

    #[cfg(feature = "replay")]
    pub fn load_from_data(&mut self, texture: Texture, data: Vec<u8>) {
        assert!(self.storage.texture.is_none());
        match self.backend {
            GpuCacheBackend::PixelBuffer { ref mut rows, .. } => {
                let dim = texture.get_dimensions();
                let blocks = unsafe {
                    std::slice::from_raw_parts(
                        data.as_ptr() as *const GpuBlockData,
                        data.len() / mem::size_of::<GpuBlockData>(),
                    )
                };
                // fill up the CPU cache from the contents we just loaded
                rows.clear();
                rows.extend((0 .. dim.height).map(|_| CacheRow::new()));
                let chunks = blocks.chunks(super::MAX_VERTEX_TEXTURE_WIDTH);
                debug_assert_eq!(chunks.len(), rows.len());
                for (row, chunk) in rows.iter_mut().zip(chunks) {
                    row.cpu_blocks.copy_from_slice(chunk);
                }
            }
            GpuCacheBackend::Scatter { .. } => {}
        }
        self.storage.texture = Some(texture);
    }

    pub fn report_memory_to(&self, report: &mut MemoryReport, size_op_funs: &MallocSizeOfOps) {
        if let GpuCacheBackend::PixelBuffer{ref rows, ..} = self.backend {
            for row in rows.iter() {
                report.gpu_cache_cpu_mirror += unsafe { (size_op_funs.size_of_op)(row.cpu_blocks.as_ptr() as *const _) };
            }
        }

        self.storage.report_memory_to(report);
    }

    pub fn gpu_size_in_bytes(&self) -> usize {
        self.storage.gpu_size_in_bytes()
    }
}

impl RendererGpuCache {
    pub fn new_gl(
        device: &mut Device,
        use_scatter: bool,
    ) -> Result<Self, super::RendererError> {
        Ok(Self::Gl(GpuCacheTexture::new(device, use_scatter)?))
    }

    fn gl(&self) -> &GpuCacheTexture {
        match self {
            Self::Gl(cache) => cache,
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => unreachable!("wgpu gpu cache backend is not wired yet"),
        }
    }

    fn gl_mut(&mut self) -> &mut GpuCacheTexture {
        match self {
            Self::Gl(cache) => cache,
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => unreachable!("wgpu gpu cache backend is not wired yet"),
        }
    }

    pub fn uses_scatter_updates(&self) -> bool {
        matches!(self.gl().backend, GpuCacheBackend::Scatter { .. })
    }

    fn texture(&self) -> &Texture {
        self.gl().storage.texture.as_ref().unwrap()
    }

    pub fn get_height(&self) -> i32 {
        self.gl().get_height()
    }

    #[cfg(feature = "capture")]
    pub fn get_texture(&self) -> &Texture {
        self.texture()
    }

    fn prepare_for_updates(
        &mut self,
        device: &mut Device,
        total_block_count: usize,
        max_height: i32,
    ) {
        self.gl_mut()
            .prepare_for_updates(device, total_block_count, max_height);
    }

    pub fn invalidate(&mut self) {
        self.gl_mut().invalidate();
    }

    fn update(&mut self, device: &mut Device, updates: &GpuCacheUpdateList) {
        self.gl_mut().update(device, updates);
    }

    fn flush(&mut self, device: &mut Device, pbo_pool: &mut UploadPBOPool) -> usize {
        self.gl_mut().flush(device, pbo_pool)
    }

    pub fn deinit(self, device: &mut Device) {
        match self {
            Self::Gl(cache) => cache.deinit(device),
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => {}
        }
    }

    #[cfg(feature = "replay")]
    pub fn remove_texture(&mut self, device: &mut Device) {
        self.gl_mut().remove_texture(device);
    }

    #[cfg(feature = "replay")]
    pub fn load_from_data(&mut self, texture: Texture, data: Vec<u8>) {
        self.gl_mut().load_from_data(texture, data);
    }

    pub fn report_memory_to(&self, report: &mut MemoryReport, size_op_funs: &MallocSizeOfOps) {
        self.gl().report_memory_to(report, size_op_funs);
    }

    pub fn gpu_size_in_bytes(&self) -> usize {
        self.gl().gpu_size_in_bytes()
    }
}

impl super::Renderer {
    pub fn update_gpu_cache(&mut self) {
        let _gm = self.gpu_profiler.start_marker("gpu cache update");

        // For an artificial stress test of GPU cache resizing,
        // always pass an extra update list with at least one block in it.
        let gpu_cache_height = self.gpu_cache_texture.get_height();
        if gpu_cache_height != 0 && GPU_CACHE_RESIZE_TEST {
            self.pending_gpu_cache_updates.push(GpuCacheUpdateList {
                frame_id: FrameId::INVALID,
                clear: false,
                height: gpu_cache_height,
                blocks: vec![[1f32; 4].into()],
                updates: Vec::new(),
                debug_commands: Vec::new(),
            });
        }

        let (updated_blocks, max_requested_height) = self
            .pending_gpu_cache_updates
            .iter()
            .fold((0, gpu_cache_height), |(count, height), list| {
                (count + list.blocks.len(), cmp::max(height, list.height))
            });

        if max_requested_height > self.get_max_texture_size() && !self.gpu_cache_overflow {
            self.gpu_cache_overflow = true;
            self.renderer_errors.push(super::RendererError::MaxTextureSize);
        }

        // Note: if we decide to switch to scatter-style GPU cache update
        // permanently, we can have this code nicer with `BufferUploader` kind
        // of helper, similarly to how `TextureUploader` API is used.
        self.gpu_cache_texture.prepare_for_updates(
            self.device.as_mut().unwrap(),
            updated_blocks,
            max_requested_height,
        );

        for update_list in self.pending_gpu_cache_updates.drain(..) {
            assert!(update_list.height <= max_requested_height);
            if update_list.frame_id > self.gpu_cache_frame_id {
                self.gpu_cache_frame_id = update_list.frame_id
            }
            self.gpu_cache_texture
                .update(self.device.as_mut().unwrap(), &update_list);
        }

        self.profile.start_time(profiler::GPU_CACHE_UPLOAD_TIME);
        let updated_rows = self.gpu_cache_texture.flush(
            self.device.as_mut().unwrap(),
            self.upload_state.gl_pools_mut().0
        );
        self.gpu_cache_upload_time += self.profile.end_time(profiler::GPU_CACHE_UPLOAD_TIME);

        self.profile.set(profiler::GPU_CACHE_ROWS_UPDATED, updated_rows);
        self.profile.set(profiler::GPU_CACHE_BLOCKS_UPDATED, updated_blocks);
    }

    pub fn prepare_gpu_cache(
        &mut self,
        deferred_resolves: &[DeferredResolve],
    ) -> Result<(), super::RendererError> {
        self.profile.start_time(profiler::GPU_CACHE_PREPARE_TIME);

        if self.pending_gpu_cache_clear {
            let use_scatter = self.gpu_cache_texture.uses_scatter_updates();
            let new_cache = match RendererGpuCache::new_gl(self.device.as_mut().unwrap(), use_scatter) {
                Ok(cache) => cache,
                Err(err) => {
                    self.profile.end_time(profiler::GPU_CACHE_PREPARE_TIME);
                    return Err(err);
                }
            };
            let old_cache = mem::replace(&mut self.gpu_cache_texture, new_cache);
            old_cache.deinit(self.device.as_mut().unwrap());
            self.pending_gpu_cache_clear = false;
        }

        let deferred_update_list = self.update_deferred_resolves(deferred_resolves);
        self.pending_gpu_cache_updates.extend(deferred_update_list);

        self.update_gpu_cache();

        // Note: the texture might have changed during the `update`,
        // so we need to bind it here.
        self.device.as_mut().unwrap().bind_texture(
            super::TextureSampler::GpuCache,
            self.gpu_cache_texture.texture(),
            Swizzle::default(),
        );

        self.profile.end_time(profiler::GPU_CACHE_PREPARE_TIME);

        Ok(())
    }

    pub fn read_gpu_cache(&mut self) -> (DeviceIntSize, Vec<u8>) {
        let texture = self.gpu_cache_texture.texture();
        let size = device_size_as_framebuffer_size(texture.get_dimensions());
        let mut texels = vec![0; (size.width * size.height * 16) as usize];
        self.device.as_mut().unwrap().begin_frame();
        self.device.as_mut().unwrap().bind_read_target(ReadTarget::from_texture(texture));
        self.device.as_mut().unwrap().read_pixels_into(
            size.into(),
            api::ImageFormat::RGBAF32,
            &mut texels,
        );
        self.device.as_mut().unwrap().reset_read_target();
        self.device.as_mut().unwrap().end_frame();
        (texture.get_dimensions(), texels)
    }
}
