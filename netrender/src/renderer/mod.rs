/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `Renderer` shell — the public entry for `prepare()` + `render()`.
//!
//! Phase plan recap (each phase's invariants are still load-bearing):
//!
//! - **Phase 4** introduced depth sorting: opaques drawn front-to-back
//!   with depth write ON (early-Z benefit), alphas drawn back-to-front
//!   with depth write OFF and premultiplied-alpha blend. The depth
//!   texture lives in `PreparedFrame` so `render()` stays upload-free
//!   (axiom 13).
//! - **Phase 5** added image primitives via `brush_image`. The
//!   `Renderer` owns an `ImageCache` keyed by `ImageKey` and a
//!   nearest-clamp sampler.
//! - **Phase 6** added the render-task graph (separable Gaussian blur
//!   landed as the first consumer) and an `insert_image_gpu` bridge
//!   that lets graph outputs participate in the scene-compositing
//!   path.
//! - **Phase 7A/7B/7C** added the tile cache: invalidation algorithm,
//!   per-tile rendering, and `prepare()` routing through the cache
//!   when `NetrenderOptions::tile_cache_size = Some(_)`. Composite
//!   draws are one `brush_image_alpha` per cached tile; pixel result
//!   is equivalent to the direct path within ±2/255.
//! - **Phase 8A-D** added analytic gradients (linear, radial, conic)
//!   and unified them into one `brush_gradient` pipeline + N-stop ramp
//!   in 8D.
//! - **Phase 9A/9B/9C** added the rounded-rect clip-mask shader
//!   `cs_clip_rectangle`. Mask integration uses the render-graph +
//!   image-cache + `brush_image` chain (no per-primitive clip-mask
//!   binding yet — that's Phase 11+). Box-shadow chains
//!   `cs_clip_rectangle → brush_blur (H + V)`.

pub(crate) mod init;

use std::sync::{Arc, Mutex};

use netrender_device::{
    ColorAttachment, DepthAttachment, DrawIntent, RenderPassTarget, WgpuDevice,
};

use crate::batch::{
    FrameResources, GradientPipelines, build_gradient_batch, build_image_batch, build_rect_batch,
    make_per_frame_buf_for_rect, make_transforms_buf, write_image_instance,
};
use crate::image_cache::ImageCache;
use crate::scene::{GradientKind, ImageKey, Scene};
use crate::tile_cache::{TileCache, TileCoord, aabb_intersects, world_aabb};

pub struct Renderer {
    pub wgpu_device: WgpuDevice,
    pub(crate) image_cache: Mutex<ImageCache>,
    pub(crate) nearest_sampler: wgpu::Sampler,
    /// Bilinear-clamp sampler for blur and filter tasks in the render graph.
    pub bilinear_sampler: wgpu::Sampler,
    /// Phase 7C: when present, `prepare()` routes through the tile cache
    /// (renders dirty tiles, composites them via `brush_image_alpha`).
    /// Configured at construction via `NetrenderOptions::tile_cache_size`.
    pub(crate) tile_cache: Option<Mutex<TileCache>>,
    /// Phase 7' — vello-backed tile rasterizer. Constructed at init
    /// when `NetrenderOptions::enable_vello` is true. Independent of
    /// the existing batched WGSL `prepare()` / `render()` pipeline;
    /// `Renderer::render_vello` routes through this instead.
    pub(crate) vello_rasterizer: Option<Mutex<crate::vello_tile_rasterizer::VelloTileRasterizer>>,
}

/// Retained per-frame resources whose lifetime needs to span the frame.
#[derive(Default)]
pub struct ResourceRefs {}

/// Prepare-phase output. Holds the sorted draw list and the depth
/// texture created for this frame's pass. Both must outlive `render()`.
pub struct PreparedFrame {
    /// All draw intents: opaque rects (front-to-back), opaque images,
    /// alpha rects (back-to-front), alpha images.
    pub draws: Vec<DrawIntent>,
    /// Depth texture for the main pass (Depth32Float, discard-on-store).
    pub depth_tex: wgpu::Texture,
    /// Default view into `depth_tex`; borrowed by `render()`.
    pub depth_view: wgpu::TextureView,
    pub retained: ResourceRefs,
}

/// Embedder-supplied target for one frame.
pub struct FrameTarget<'a> {
    pub view: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

/// Per-frame load policy on the color attachment.
pub enum ColorLoad {
    Clear(wgpu::Color),
    Load,
}

impl Default for ColorLoad {
    fn default() -> Self {
        Self::Clear(wgpu::Color::TRANSPARENT)
    }
}

impl Renderer {
    const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
    const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
    /// Phase 7B tile texture format: linear (no sRGB curve in the cache),
    /// composited into the sRGB framebuffer in 7C. See the design plan's
    /// "Defaults" subsection under Phase 7.
    const TILE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    /// Build a [`PreparedFrame`] from a [`Scene`].
    ///
    /// Uploads any new image sources to the GPU cache, builds pipelines
    /// (cached by format key), sorts and uploads instance data. All GPU
    /// writes happen here so [`Renderer::render`] stays upload-free (axiom 13).
    ///
    /// Draw order: opaque rects → opaque images → alpha rects → alpha images.
    ///
    /// Phase 7C: when this `Renderer` was constructed with
    /// `NetrenderOptions::tile_cache_size = Some(_)`, `prepare()` routes
    /// through the tile cache instead — dirty tiles render into per-tile
    /// `Arc<wgpu::Texture>` cache entries, and the returned draw list is
    /// one `brush_image_alpha` composite draw per tile. The framebuffer
    /// pixel result is equivalent (within ±2/255 tolerance) to the
    /// direct path; the win is that re-running `prepare()` on an
    /// unchanged scene re-renders zero tiles.
    pub fn prepare(&self, scene: &Scene) -> PreparedFrame {
        if let Some(tc_mutex) = &self.tile_cache {
            let mut tc = tc_mutex.lock().expect("tile_cache lock");
            self.prepare_tiled(scene, &mut tc)
        } else {
            self.prepare_direct(scene)
        }
    }

    /// Direct (no-tile-cache) prepare path. Pre-7C behavior.
    fn prepare_direct(&self, scene: &Scene) -> PreparedFrame {
        let opaque_pipe = self
            .wgpu_device
            .ensure_brush_rect_solid_opaque(Self::COLOR_FORMAT, Self::DEPTH_FORMAT);
        let alpha_pipe = self
            .wgpu_device
            .ensure_brush_rect_solid_alpha(Self::COLOR_FORMAT, Self::DEPTH_FORMAT);
        let image_opaque_pipe = self
            .wgpu_device
            .ensure_brush_image_opaque(Self::COLOR_FORMAT, Self::DEPTH_FORMAT);
        let image_alpha_pipe = self
            .wgpu_device
            .ensure_brush_image_alpha(Self::COLOR_FORMAT, Self::DEPTH_FORMAT);
        // Phase 8D: one gradient pipeline per (kind, alpha_class) — 6 total.
        let gradient_pipes = self.ensure_gradient_pipelines(Self::COLOR_FORMAT);

        let depth_tex = self.wgpu_device.core.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("netrender depth"),
            size: wgpu::Extent3d {
                width: scene.viewport_width,
                height: scene.viewport_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Upload any new image sources to the GPU cache.
        {
            let mut cache = self.image_cache.lock().expect("image_cache lock");
            for (key, data) in &scene.image_sources {
                cache.get_or_upload(
                    *key,
                    data,
                    &self.wgpu_device.core.device,
                    &self.wgpu_device.core.queue,
                );
            }
        }

        let device = &self.wgpu_device.core.device;
        let queue = &self.wgpu_device.core.queue;

        // One shared transforms + per_frame upload for the whole frame.
        let frame_res = FrameResources::new(scene, device, queue);

        // Direct path: no per-prim filter (every primitive contributes).
        let rect_draws = build_rect_batch(
            scene, device, queue, &opaque_pipe, &alpha_pipe, &frame_res, None,
        );

        let image_draws = {
            let cache = self.image_cache.lock().expect("image_cache lock");
            build_image_batch(
                scene, device, queue,
                &image_opaque_pipe, &image_alpha_pipe,
                &cache, &self.nearest_sampler, &frame_res,
                None,
            )
        };

        // Phase 8D: one unified gradient batch — push order preserved
        // across linear / radial / conic kinds.
        let gradient_draws =
            build_gradient_batch(scene, device, queue, &gradient_pipes, &frame_res, None);

        // Concat: rects → images → gradients. Each batch emits opaques
        // first then alphas; cross-batch correctness comes from the
        // unified z_depth assignment.
        let draws = merge_draw_order(rect_draws, image_draws, gradient_draws);

        PreparedFrame {
            draws,
            depth_tex,
            depth_view,
            retained: ResourceRefs::default(),
        }
    }

    /// Tile-cache prepare path: invalidate + render dirty tiles, then
    /// build one `brush_image_alpha` composite draw per cached tile.
    fn prepare_tiled(&self, scene: &Scene, tc: &mut TileCache) -> PreparedFrame {
        let device = &self.wgpu_device.core.device;
        let queue = &self.wgpu_device.core.queue;

        // Build the shared transforms buffer once and reuse it across
        // both the tile rendering pass and the composite pass.
        // wgpu::Buffer is Arc-internal so the clone into each
        // FrameResources is cheap.
        let transforms_buf = make_transforms_buf(scene, device, queue);

        // Step 1: dirty tiles re-render into their cached textures.
        let _dirty = self.render_dirty_tiles_with_transforms(scene, tc, &transforms_buf);

        // Step 2: build composite draws — one brush_image_alpha per tile.
        let composite_pipe = self
            .wgpu_device
            .ensure_brush_image_alpha(Self::COLOR_FORMAT, Self::DEPTH_FORMAT);

        // Composite uses the FULL framebuffer projection (not tile-local) —
        // each tile's rect is given in world coords, mapped through the
        // viewport projection to the framebuffer.
        let per_frame_buf = make_per_frame_buf_for_rect(
            [0.0, 0.0, scene.viewport_width as f32, scene.viewport_height as f32],
            device,
            queue,
        );

        let mut composite_draws = Vec::with_capacity(tc.tiles.len());
        for tile in tc.tiles.values() {
            if let Some(draw) = self.build_tile_composite_draw(
                tile,
                &composite_pipe,
                &transforms_buf,
                &per_frame_buf,
                device,
                queue,
            ) {
                composite_draws.push(draw);
            }
        }

        // Step 3: depth texture for the main pass (composite draws use
        // brush_image_alpha which depth-tests but doesn't depth-write,
        // so all tiles at z=0.5 pass against a 1.0-cleared buffer).
        let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("netrender depth (tiled)"),
            size: wgpu::Extent3d {
                width: scene.viewport_width,
                height: scene.viewport_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_tex.create_view(&wgpu::TextureViewDescriptor::default());

        PreparedFrame {
            draws: composite_draws,
            depth_tex,
            depth_view,
            retained: ResourceRefs::default(),
        }
    }

    /// Build one `brush_image_alpha` draw that samples `tile.texture`
    /// and places it at `tile.world_rect` in framebuffer coordinates.
    /// Returns `None` if the tile has no cached texture (un-rendered tile).
    fn build_tile_composite_draw(
        &self,
        tile: &crate::tile_cache::Tile,
        pipe: &netrender_device::BrushImagePipeline,
        transforms_buf: &wgpu::Buffer,
        per_frame_buf: &wgpu::Buffer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Option<DrawIntent> {
        let texture = tile.texture.as_ref()?;

        // One ImageInstance per tile. Composite-specific values:
        // full UV (sample whole tile), no tint, no clip (NO_CLIP-style
        // infinities), identity transform, z = 0.5 (tiles don't overlap
        // each other so any consistent z works against the depth-cleared
        // framebuffer). Layout shared with `emit_image_draws` via the
        // `write_image_instance` helper.
        let mut bytes = Vec::with_capacity(80);
        write_image_instance(
            &mut bytes,
            tile.world_rect,
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [f32::NEG_INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::INFINITY],
            0,
            0.5,
        );
        debug_assert_eq!(bytes.len(), 80);

        let instances_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile composite instance"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&instances_buf, 0, &bytes);

        let tex_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile composite bind group"),
            layout: &pipe.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: instances_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: transforms_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: per_frame_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.nearest_sampler),
                },
            ],
        });

        Some(DrawIntent {
            pipeline: pipe.pipeline.clone(),
            bind_group,
            vertex_buffers: vec![],
            vertex_range: 0..4,
            instance_range: 0..1,
            dynamic_offsets: Vec::new(),
            push_constants: Vec::new(),
        })
    }

    /// Borrow the tile cache (when `Renderer` was created with
    /// `NetrenderOptions::tile_cache_size = Some(_)`). Useful for tests
    /// that need to query `dirty_count_last_invalidate()` after a
    /// `prepare()` call.
    pub fn tile_cache(&self) -> Option<&Mutex<TileCache>> {
        self.tile_cache.as_ref()
    }

    /// Phase 8D: ensure the 6 cached `brush_gradient` pipelines (3
    /// kinds × 2 alpha classes) for the given color format. Pipelines
    /// are cached on `WgpuDevice` by `(color, depth, alpha, kind)`,
    /// so subsequent calls with the same format return the same Arcs.
    fn ensure_gradient_pipelines(
        &self,
        color_format: wgpu::TextureFormat,
    ) -> GradientPipelines {
        let kinds = [GradientKind::Linear, GradientKind::Radial, GradientKind::Conic];
        GradientPipelines {
            opaque: kinds.map(|k| {
                self.wgpu_device
                    .ensure_brush_gradient_opaque(color_format, Self::DEPTH_FORMAT, k)
            }),
            alpha: kinds.map(|k| {
                self.wgpu_device
                    .ensure_brush_gradient_alpha(color_format, Self::DEPTH_FORMAT, k)
            }),
        }
    }

    /// Insert a pre-existing GPU texture into the image cache, making it
    /// available for compositing via [`Scene::push_image_full`] in the next
    /// `prepare()` call. The typical use is injecting render-graph outputs
    /// (blur, filter) as image primitives in the main scene pass.
    pub fn insert_image_gpu(&self, key: ImageKey, texture: Arc<wgpu::Texture>) {
        self.image_cache.lock().expect("image_cache lock").insert_gpu(key, texture);
    }

    /// Phase 7B: invalidate `tile_cache` against `scene` and render every
    /// dirty tile into its cached `Arc<wgpu::Texture>`. Returns the dirty
    /// tile coords (same list `TileCache::invalidate` returned).
    ///
    /// Each dirty tile gets a fresh `Rgba8Unorm` texture and is rendered
    /// using the existing `brush_rect_solid` / `brush_image` pipelines
    /// with a tile-local orthographic projection. All tiles share one
    /// `Depth32Float` texture (cleared per pass) and one `transforms`
    /// storage buffer; only the per-frame projection differs between
    /// tiles. Phase 7C composites these textures into the framebuffer.
    pub fn render_dirty_tiles(
        &self,
        scene: &Scene,
        tile_cache: &mut TileCache,
    ) -> Vec<TileCoord> {
        let device = &self.wgpu_device.core.device;
        let queue = &self.wgpu_device.core.queue;
        let transforms_buf = make_transforms_buf(scene, device, queue);
        self.render_dirty_tiles_with_transforms(scene, tile_cache, &transforms_buf)
    }

    /// Phase 7B implementation that takes a pre-built `transforms_buf`
    /// so `prepare_tiled` can share one buffer between tile rendering
    /// and the subsequent composite pass. Public callers go through
    /// `render_dirty_tiles` (which builds its own).
    fn render_dirty_tiles_with_transforms(
        &self,
        scene: &Scene,
        tile_cache: &mut TileCache,
        transforms_buf: &wgpu::Buffer,
    ) -> Vec<TileCoord> {
        let dirty = tile_cache.invalidate(scene);
        if dirty.is_empty() {
            return dirty;
        }

        let device = &self.wgpu_device.core.device;
        let queue = &self.wgpu_device.core.queue;
        let tile_size = tile_cache.tile_size();

        // Pipelines (cached by (color_format, depth_format, alpha_blend))
        let opaque_pipe = self
            .wgpu_device
            .ensure_brush_rect_solid_opaque(Self::TILE_FORMAT, Self::DEPTH_FORMAT);
        let alpha_pipe = self
            .wgpu_device
            .ensure_brush_rect_solid_alpha(Self::TILE_FORMAT, Self::DEPTH_FORMAT);
        let image_opaque_pipe = self
            .wgpu_device
            .ensure_brush_image_opaque(Self::TILE_FORMAT, Self::DEPTH_FORMAT);
        let image_alpha_pipe = self
            .wgpu_device
            .ensure_brush_image_alpha(Self::TILE_FORMAT, Self::DEPTH_FORMAT);
        // Phase 8D: 6 cached gradient pipelines for the tile format.
        let gradient_pipes = self.ensure_gradient_pipelines(Self::TILE_FORMAT);

        // Upload any new image sources (matches prepare()'s contract).
        {
            let mut cache = self.image_cache.lock().expect("image_cache lock");
            for (key, data) in &scene.image_sources {
                cache.get_or_upload(*key, data, device, queue);
            }
        }

        // One depth texture shared across all tile passes (cleared per pass).
        let depth_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile depth (shared)"),
            size: wgpu::Extent3d {
                width: tile_size,
                height: tile_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // `transforms_buf` is supplied by the caller — `prepare_tiled`
        // shares one buffer between tile rendering and the composite
        // pass. wgpu::Buffer is Arc-internal so cloning into each
        // tile's FrameResources is cheap.

        let mut encoder = self.wgpu_device.create_encoder("tile cache pass");

        // Hold the image-cache lock across all tile passes; build_image_batch
        // reads it for each tile.
        let image_cache = self.image_cache.lock().expect("image_cache lock");

        for &coord in &dirty {
            let tile_world_rect = tile_cache
                .tiles
                .get(&coord)
                .expect("dirty tile present in cache")
                .world_rect;

            let per_frame = make_per_frame_buf_for_rect(tile_world_rect, device, queue);
            let frame_res = FrameResources {
                transforms: transforms_buf.clone(),
                per_frame,
            };

            // Per-tile primitive filters: include only primitives whose
            // world AABB intersects the tile rect. NDC clipping is the
            // safety net for any false positive (a prim that's slightly
            // larger than its AABB suggests still gets clipped); a
            // false negative would manifest as missing pixels and
            // would be caught by the pixel-equivalence receipt.
            let rect_filter = |i: usize| {
                let r = &scene.rects[i];
                let aabb = world_aabb([r.x0, r.y0, r.x1, r.y1], r.transform_id, scene);
                aabb_intersects(aabb, tile_world_rect)
            };
            let image_filter = |i: usize| {
                let img = &scene.images[i];
                let aabb = world_aabb([img.x0, img.y0, img.x1, img.y1], img.transform_id, scene);
                aabb_intersects(aabb, tile_world_rect)
            };
            let gradient_filter = |i: usize| {
                let g = &scene.gradients[i];
                let aabb = world_aabb([g.x0, g.y0, g.x1, g.y1], g.transform_id, scene);
                aabb_intersects(aabb, tile_world_rect)
            };

            let rect_draws = build_rect_batch(
                scene, device, queue, &opaque_pipe, &alpha_pipe, &frame_res,
                Some(&rect_filter),
            );
            let image_draws = build_image_batch(
                scene,
                device,
                queue,
                &image_opaque_pipe,
                &image_alpha_pipe,
                &image_cache,
                &self.nearest_sampler,
                &frame_res,
                Some(&image_filter),
            );
            let gradient_draws = build_gradient_batch(
                scene, device, queue, &gradient_pipes, &frame_res,
                Some(&gradient_filter),
            );
            let mut draws = rect_draws;
            draws.extend(image_draws);
            draws.extend(gradient_draws);

            let tile_tex = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("tile color"),
                size: wgpu::Extent3d {
                    width: tile_size,
                    height: tile_size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: Self::TILE_FORMAT,
                // RENDER_ATTACHMENT: we draw into it.
                // TEXTURE_BINDING: 7C samples it via brush_image.
                // COPY_SRC: tests / debugging can read it back.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
            let tile_view = tile_tex.create_view(&wgpu::TextureViewDescriptor::default());

            let color = ColorAttachment::clear(&tile_view, wgpu::Color::TRANSPARENT);
            let depth = DepthAttachment::clear(&depth_view, 1.0).discard();
            self.wgpu_device.encode_pass(
                &mut encoder,
                RenderPassTarget {
                    label: "tile pass",
                    color,
                    depth: Some(depth),
                },
                &draws,
            );

            tile_cache
                .tiles
                .get_mut(&coord)
                .expect("dirty tile still present")
                .texture = Some(tile_tex);
        }

        drop(image_cache);
        self.wgpu_device.submit(encoder);

        dirty
    }

    /// Render a [`PreparedFrame`] into the embedder-supplied
    /// [`FrameTarget`]. One render pass; no uploads (axiom 13).
    pub fn render(&self, prepared: &PreparedFrame, target: FrameTarget<'_>, load: ColorLoad) {
        let color = match load {
            ColorLoad::Clear(c) => ColorAttachment::clear(target.view, c),
            ColorLoad::Load => ColorAttachment::load(target.view),
        };
        let depth = DepthAttachment::clear(&prepared.depth_view, 1.0).discard();

        let mut encoder = self.wgpu_device.create_encoder("netrender frame");
        self.wgpu_device.encode_pass(
            &mut encoder,
            RenderPassTarget {
                label: "netrender main pass",
                color,
                depth: Some(depth),
            },
            &prepared.draws,
        );
        self.wgpu_device.submit(encoder);
    }

    /// Phase 7' — render `scene` into `target_view` via the
    /// vello-backed tile rasterizer.
    ///
    /// The renderer must have been constructed with
    /// `NetrenderOptions { enable_vello: true, tile_cache_size:
    /// Some(_), .. }`. The tile cache is shared with the existing
    /// batched `prepare()` path, so callers can switch between the
    /// two pipelines per frame without losing tile-level
    /// invalidation state.
    ///
    /// Single-method entry point: invalidates the tile cache against
    /// `scene`, rebuilds dirty tiles' vello::Scenes, composes them
    /// via `Scene::append` with per-tile clip layers, and issues one
    /// `vello::Renderer::render_to_texture` call. See
    /// `vello_tile_rasterizer::VelloTileRasterizer::render` for the
    /// step-by-step contract.
    ///
    /// # Panics
    ///
    /// - If `enable_vello` was `false` at construction (no rasterizer
    ///   available).
    /// - If `tile_cache_size` was `None` at construction (the vello
    ///   rasterizer requires the cache; this is also enforced at
    ///   construction time as `RendererError::VelloRequiresTileCache`,
    ///   so reaching this panic means the renderer was malformed).
    /// - If a vello render error occurs (returns the error wrapped
    ///   in a panic message; matches the existing `render()` API
    ///   shape, which doesn't return a Result).
    /// Number of tiles whose vello::Scenes were rebuilt during the
    /// most recent `render_vello` call. `0` after a no-op frame
    /// (unchanged scene). Returns `None` if the renderer was
    /// constructed without `enable_vello`.
    pub fn vello_last_dirty_count(&self) -> Option<usize> {
        let rast_mutex = self.vello_rasterizer.as_ref()?;
        let rast = rast_mutex.lock().expect("vello_rasterizer lock");
        Some(rast.last_dirty_count())
    }

    /// Number of tile-Scenes currently held in the vello rasterizer's
    /// cache. Returns `None` if `enable_vello` was false.
    pub fn vello_cached_tile_count(&self) -> Option<usize> {
        let rast_mutex = self.vello_rasterizer.as_ref()?;
        let rast = rast_mutex.lock().expect("vello_rasterizer lock");
        Some(rast.cached_tile_count())
    }

    pub fn render_vello(&self, scene: &Scene, target_view: &wgpu::TextureView) {
        let rast_mutex = self
            .vello_rasterizer
            .as_ref()
            .expect("Renderer::render_vello requires NetrenderOptions::enable_vello = true");
        let tc_mutex = self
            .tile_cache
            .as_ref()
            .expect("Renderer::render_vello requires NetrenderOptions::tile_cache_size = Some(_)");

        let mut rast = rast_mutex.lock().expect("vello_rasterizer lock");
        let mut tc = tc_mutex.lock().expect("tile_cache lock");
        rast.render(scene, &mut tc, target_view)
            .unwrap_or_else(|e| panic!("vello render_to_texture failed: {:?}", e));
    }
}

/// Concatenate the per-family draw lists. Each batch emits opaques
/// first then alphas; cross-batch correctness comes from the unified
/// `n_total`-based z_depth so the front-most primitive (any family)
/// wins the depth test. Family painter order: rects → images →
/// gradients (linear / radial / conic interleaved by user push order
/// inside `gradient_draws`).
fn merge_draw_order(
    mut rect_draws: Vec<DrawIntent>,
    image_draws: Vec<DrawIntent>,
    gradient_draws: Vec<DrawIntent>,
) -> Vec<DrawIntent> {
    rect_draws.extend(image_draws);
    rect_draws.extend(gradient_draws);
    rect_draws
}

#[derive(Debug)]
pub enum RendererError {
    WgpuFeaturesMissing(wgpu::Features),
    /// `NetrenderOptions::enable_vello = true` requires
    /// `tile_cache_size = Some(_)`. The vello rasterizer holds the
    /// per-tile vello::Scene cache; without a tile cache there's
    /// nothing for it to cache against.
    VelloRequiresTileCache,
    /// `vello::Renderer` construction failed during
    /// `create_netrender_instance`. The wrapped string is vello's
    /// error formatted via `{:?}` (vello::Error doesn't implement
    /// std::error::Error in 0.8 — the string is informational only).
    VelloInit(String),
}
