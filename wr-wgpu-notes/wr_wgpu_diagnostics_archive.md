# wgpu Debug Diagnostics Archive

Diagnostic instrumentation added during the DPR=2 text scaling bug investigation
(2026-04-01 to 2026-04-02). Preserved here for reference before removal from source.

Bug: wgpu text rendered at 2x CSS size with wrong colors at DPR=2.
Root cause: Missing `GLYPH_TRANSFORM` shader variant in `batch_key_to_pipeline_key()`.

## picture.rs

### DPS_DIAG (line ~5423)

Logged surface device_pixel_scale and world_scale_factors for each surface.
Limit: 50 firings. Key finding: DPS=1.0 for both GL and wgpu paths.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering};
    static DPS_LOG: AtomicU32 = AtomicU32::new(0);
    if DPS_LOG.fetch_add(1, Ordering::Relaxed) < 50 {
        info!("wgpu DPS_DIAG: surface[{}].device_pixel_scale={:.3}, world_scale=({:.3},{:.3})",
              surface_index.0,
              device_pixel_scale.0,
              frame_state.surfaces[surface_index.0].world_scale_factors.0,
              frame_state.surfaces[surface_index.0].world_scale_factors.1,
        );
    }
}
```

### WSF_DIAG (line ~7030)

Logged world_scale_factors when parent_surface_index=None (root tile cache).
Limit: 5 firings. Key finding: surface_node == root_ref == SpatialNodeIndex(0), scale=(1,1).

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering};
    static WSF_LOG: AtomicU32 = AtomicU32::new(0);
    if WSF_LOG.fetch_add(1, Ordering::Relaxed) < 5 {
        info!("wgpu WSF_DIAG: parent=None surface_node={:?} root_ref={:?} scale=({:.3},{:.3}) composite_mode={:?}",
              surface_spatial_node_index,
              frame_context.spatial_tree.root_reference_frame_index(),
              scale_factors.0, scale_factors.1,
              self.composite_mode.as_ref().map(|m| std::mem::discriminant(m)));
    }
}
```

### TILECACHE_DPS (line ~7111)

Logged scaling_factor computation in TileCache branch. Limit: 3 firings.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering};
    static TC_LOG: AtomicU32 = AtomicU32::new(0);
    if TC_LOG.fetch_add(1, Ordering::Relaxed) < 3 {
        info!("wgpu TILECACHE_DPS: scaling_factor={:.3} world_scale=({:.3},{:.3}) min_scale={:.3} dps={:.3}",
              scaling_factor, world_scale_factors.0, world_scale_factors.1, min_scale, device_pixel_scale.0);
    }
}
```

### SNAP_DPS (line ~7142)

Logged local_scale when snapping branch taken. Limit: 3 firings.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering};
    static SNAP_LOG: AtomicU32 = AtomicU32::new(0);
    if SNAP_LOG.fetch_add(1, Ordering::Relaxed) < 3 {
        info!("wgpu SNAP_DPS: local_scale=({:.3},{:.3}) surface_node={:?} prim_node={:?}",
              local_scale.0, local_scale.1,
              surface_spatial_node_index, self.spatial_node_index);
    }
}
```

## renderer/mod.rs

### DIAG_COUNT / frame diag (line ~1551)

Logged frame data statistics for first 3 non-empty frames.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering};
    static DIAG_COUNT: AtomicU32 = AtomicU32::new(0);
    let pic_targets: usize = frame.passes.iter().map(|p| p.picture_cache.len()).sum();
    if pic_targets > 0 && DIAG_COUNT.fetch_add(1, Ordering::Relaxed) < 3 {
        info!("wgpu frame diag: passes={}, prim_headers_f={}, prim_headers_i={}, transforms={}, render_tasks={}, pic_cache_targets={}, has_been_rendered={}",
              frame.passes.len(),
              frame.prim_headers.headers_float.len(),
              frame.prim_headers.headers_int.len(),
              frame.transform_palette.len(),
              frame.render_tasks.task_data.len(),
              pic_targets,
              frame.has_been_rendered,
        );
    }
}
```

### COMP_LOG / COMP_TILE (line ~1642)

Logged composite tile rects. Limit: 5 firings.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering as AO4};
    static COMP_LOG: AtomicU32 = AtomicU32::new(0);
    if COMP_LOG.fetch_add(1, AO4::Relaxed) < 5 {
        info!("wgpu COMP_TILE[{}]: local_rect={:?} tile_rect={:?} clip_rect={:?} scale=({:.3},{:.3}) surface={:?}",
              tile_idx, tile.local_rect, tile_rect, clip_rect,
              transform.scale.x, transform.scale.y,
              std::mem::discriminant(&tile.surface));
    }
}
```

### TILE_DUMPED (line ~1762)

One-shot: read back first textured composite tile and saved as PPM.

```rust
{
    use std::sync::atomic::{AtomicBool, Ordering};
    static TILE_DUMPED: AtomicBool = AtomicBool::new(false);
    if !TILE_DUMPED.load(Ordering::Relaxed) {
        TILE_DUMPED.store(true, Ordering::Relaxed);
        // ... read_texture_pixels, save as wgpu_tile_texture.ppm
    }
}
```

### CAPTURED / frame capture (line ~1817)

One-shot: read back surface texture after compositing, saved as PPM.

```rust
if let Some(ref st) = surface_texture {
    use std::sync::atomic::{AtomicBool, Ordering};
    static CAPTURED: AtomicBool = AtomicBool::new(false);
    if !CAPTURED.load(Ordering::Relaxed) && (total_textured > 0 || !color_instances.is_empty()) {
        CAPTURED.store(true, Ordering::Relaxed);
        // ... read_surface_texture_pixels, save as wgpu_frame_capture.ppm
    }
}
```

### PIC_LOG (line ~2005)

Logged picture_cache target count per render pass. Limit: 3 firings.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering};
    static PIC_LOG: AtomicU32 = AtomicU32::new(0);
    if PIC_LOG.fetch_add(1, Ordering::Relaxed) < 3 {
        info!("wgpu pass: picture_cache targets={}", pass.picture_cache.len());
    }
}
```

### CLR_LOG (line ~2089)

Logged tile clear color. Limit: 3 firings.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering as AO3};
    static CLR_LOG: AtomicU32 = AtomicU32::new(0);
    if CLR_LOG.fetch_add(1, AO3::Relaxed) < 3 {
        info!("wgpu tile clear: r={:.3} g={:.3} b={:.3} a={:.3} (from {:?})",
              tile_clear_color.r, tile_clear_color.g,
              tile_clear_color.b, tile_clear_color.a,
              picture_target.clear_color);
    }
}
```

### BATCH_LOG (line ~2105)

Logged batch shader/config/blend details. Limit: 10 firings.

```rust
{
    use std::sync::atomic::{AtomicU32, Ordering};
    static BATCH_LOG: AtomicU32 = AtomicU32::new(0);
    let n = BATCH_LOG.fetch_add(1, Ordering::Relaxed);
    if n < 10 {
        info!("wgpu batch[{}]: shader={:?} config={:?} blend={:?} instances={} target={}x{} fmt={:?} scissor={:?} is_alpha={} gpu_cache={}",
              n, shader_name, config, &$batch.key.blend_mode,
              $batch.instances.len(), target_w, target_h, target_fmt,
              scissor, $is_alpha,
              gpu_cache_view.is_some());
    }
}
```

### INST_DIAG (line ~2125)

Decoded instance data + prim headers + render tasks + transforms + glyph resources.
Limit: 5 firings. This was the most detailed diagnostic — decoded the full data chain
from instance → prim_header → render_task → transform → GPU cache.

(Full code ~90 lines, see git history)

### TILE_RB (line ~2282)

One-shot: read back rendered tile after all batches drawn, counted non-zero pixels
per channel, sampled specific pixels, saved alpha as PGM.

```rust
{
    use std::sync::atomic::{AtomicBool, Ordering};
    static TILE_RB: AtomicBool = AtomicBool::new(false);
    if !TILE_RB.load(Ordering::Relaxed) {
        TILE_RB.store(true, Ordering::Relaxed);
        // ... read_texture_pixels, count channels, save as wgpu_tile_alpha.pgm
    }
}
```

## tile_cache.rs

### TILE_CACHE_BUILD (line ~470)

Non-gated info! log on every TileCacheBuilder::build() call.

```rust
info!("wgpu TILE_CACHE_BUILD: {} tile caches, {} pictures",
      result.tile_caches.len(), tile_cache_pictures.len());
```

## File artifacts

These diagnostics could produce the following files in the working directory:

- `wgpu_tile_texture.ppm` — first composite tile texture
- `wgpu_frame_capture.ppm` — full surface after compositing
- `wgpu_tile_alpha.pgm` — alpha channel of rendered tile
