/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 6 render-task graph.
//!
//! Tasks declare their input dependencies by ID; the graph topo-sorts
//! (Kahn's algorithm) and encodes all passes into a single
//! `CommandEncoder`. Each task's output texture is allocated at execute
//! time; callers supply pre-existing textures for "external" sources
//! (uploaded images, prior frame tiles, etc.).
//!
//! Typical use: build blur or filter sub-graphs, execute them before
//! `Renderer::prepare`, insert the resulting textures into the image
//! cache via `ImageCache::insert_gpu`, then composite them as images
//! in the main scene pass.

use std::collections::{HashMap, VecDeque};

/// Stable identifier for a task's output texture within one graph execution.
pub type TaskId = u64;

/// Signature for a task's encode callback.
///
/// Receives the wgpu device (for bind group creation), the active encoder,
/// a slice of input texture views (in the order listed in `Task::inputs`,
/// filtered to those present in the output map), and the pre-created
/// output view. Should encode exactly one render pass targeting `output`.
pub type EncodeCallback = Box<
    dyn FnOnce(&wgpu::Device, &mut wgpu::CommandEncoder, &[wgpu::TextureView], &wgpu::TextureView)
        + Send,
>;

/// One node in the render-task graph.
pub struct Task {
    pub id: TaskId,
    /// Pixel dimensions of the output texture.
    pub extent: wgpu::Extent3d,
    /// Format of the output texture.
    pub format: wgpu::TextureFormat,
    /// IDs whose output textures must be ready before this task runs.
    /// External IDs (supplied via `execute`'s `externals` map) are valid
    /// here and are treated as already-complete leaf nodes.
    pub inputs: Vec<TaskId>,
    /// Encode callback: builds and submits one render pass for this task.
    pub encode: EncodeCallback,
}

/// Directed acyclic graph of render tasks.
///
/// Build with [`RenderGraph::push`], execute with [`RenderGraph::execute`].
/// A single `CommandEncoder` is used for all passes; the GPU processes
/// them in the submitted order (which matches the topo-sorted dependency
/// order).
pub struct RenderGraph {
    tasks: Vec<Task>,
}

impl RenderGraph {
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    /// Add a task to the graph. Tasks may be pushed in any order;
    /// `execute` will sort them by dependency before encoding.
    pub fn push(&mut self, task: Task) {
        self.tasks.push(task);
    }

    /// Topo-sort, allocate output textures, and encode all passes into
    /// one command submission.
    ///
    /// `externals` supplies pre-existing textures for IDs that are not
    /// registered as tasks (source images, tile inputs, etc.). They are
    /// treated as already-complete leaf nodes during the topo-sort.
    ///
    /// Returns a map of `TaskId → wgpu::Texture` containing both the
    /// externals and the newly-created task outputs. The caller can
    /// extract specific outputs by ID (e.g. to insert into the image
    /// cache for compositing in the next scene pass).
    pub fn execute(
        self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        externals: HashMap<TaskId, wgpu::Texture>,
    ) -> HashMap<TaskId, wgpu::Texture> {
        let mut task_map: HashMap<TaskId, Task> =
            self.tasks.into_iter().map(|t| (t.id, t)).collect();

        let sorted = topo_sort(&task_map, &externals);

        let mut outputs: HashMap<TaskId, wgpu::Texture> = externals;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_graph"),
        });

        for id in sorted {
            let task = task_map.remove(&id).expect("task present after topo-sort");

            let output_tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("render_graph task output"),
                size: task.extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: task.format,
                // TEXTURE_BINDING so later tasks / the image cache can read the output.
                // COPY_SRC so test code can read back pixels via read_rgba8_texture.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let output_view = output_tex.create_view(&wgpu::TextureViewDescriptor::default());

            // Collect input views in task.inputs order (skip missing externals
            // that were never registered — shouldn't happen in correct graphs).
            let input_views: Vec<wgpu::TextureView> = task
                .inputs
                .iter()
                .filter_map(|iid| outputs.get(iid))
                .map(|tex| tex.create_view(&wgpu::TextureViewDescriptor::default()))
                .collect();

            (task.encode)(device, &mut encoder, &input_views, &output_view);

            outputs.insert(id, output_tex);
        }

        queue.submit(std::iter::once(encoder.finish()));
        outputs
    }
}

/// Kahn's algorithm over the registered tasks.
///
/// External IDs are treated as already-satisfied (in-degree contribution
/// is zero from them). The sort is deterministic within a tie because
/// `VecDeque::push_back` preserves insertion order and tasks were
/// inserted in `push` order.
fn topo_sort(
    tasks: &HashMap<TaskId, Task>,
    externals: &HashMap<TaskId, wgpu::Texture>,
) -> Vec<TaskId> {
    // in_degree: how many registered-task inputs each task is still waiting on.
    let mut in_degree: HashMap<TaskId, usize> = HashMap::new();
    // rev: for each registered task output, which tasks depend on it.
    let mut rev: HashMap<TaskId, Vec<TaskId>> = HashMap::new();

    for (&id, task) in tasks {
        let degree = task
            .inputs
            .iter()
            .filter(|iid| tasks.contains_key(iid) && !externals.contains_key(iid))
            .count();
        in_degree.insert(id, degree);
        for &iid in &task.inputs {
            if tasks.contains_key(&iid) && !externals.contains_key(&iid) {
                rev.entry(iid).or_default().push(id);
            }
        }
    }

    let mut queue: VecDeque<TaskId> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut result = Vec::with_capacity(tasks.len());
    while let Some(id) = queue.pop_front() {
        result.push(id);
        if let Some(dependents) = rev.get(&id) {
            for &dep in dependents {
                let d = in_degree.get_mut(&dep).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }

    result
}
