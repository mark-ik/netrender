/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Rendering logic related to the vertex shaders and their states, uncluding
//!  - Vertex Array Objects
//!  - vertex layout descriptors
//!  - textures bound at vertex stage

use std::{marker::PhantomData, mem, num::NonZeroUsize, ops};
use api::units::*;
use crate::{
    device::{
        Device, GpuDevice, Texture, TextureFilter, TextureUploader, UploadPBOPool, VertexUsageHint, VAO,
    },
    frame_builder::Frame,
    gpu_types::{PrimitiveHeaderI, PrimitiveHeaderF, TransformData},
    internal_types::Swizzle,
    render_api::MemoryReport,
    render_task::RenderTaskData,
};

use crate::internal_types::FrameVec;

pub const VERTEX_TEXTURE_EXTRA_ROWS: i32 = 10;

pub const MAX_VERTEX_TEXTURE_WIDTH: usize = webrender_build::MAX_VERTEX_TEXTURE_WIDTH;

pub mod desc {
    use crate::device::{VertexAttribute, VertexAttributeKind, VertexDescriptor};

    pub const PRIM_INSTANCES: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[VertexAttribute {
            name: "aData",
            count: 4,
            kind: VertexAttributeKind::I32,
        }],
    };

    pub const BLUR: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aBlurRenderTaskAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aBlurSourceTaskAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aBlurDirection",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aBlurEdgeMode",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aBlurParams",
                count: 3,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const LINE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aTaskRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aLocalSize",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aWavyLineThickness",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aStyle",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aAxisSelect",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const FAST_LINEAR_GRADIENT: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aTaskRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aColor0",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aColor1",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aAxisSelect",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const LINEAR_GRADIENT: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aTaskRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aStartPoint",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aEndPoint",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aScale",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aExtendMode",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aGradientStopsAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
        ],
    };

    pub const RADIAL_GRADIENT: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aTaskRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aCenter",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aScale",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aStartRadius",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aEndRadius",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aXYRatio",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aExtendMode",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aGradientStopsAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
        ],
    };

    pub const CONIC_GRADIENT: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aTaskRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aCenter",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aScale",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aStartOffset",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aEndOffset",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aAngle",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aExtendMode",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aGradientStopsAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
        ],
    };

    pub const BORDER: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aTaskOrigin",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aColor0",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aColor1",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aFlags",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aWidths",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aRadii",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipParams1",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipParams2",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const SCALE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aScaleTargetRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aScaleSourceRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aSourceRectType",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const CLIP_RECT: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            // common clip attributes
            VertexAttribute {
                name: "aClipDeviceArea",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipOrigins",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aDevicePixelScale",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aTransformIds",
                count: 2,
                kind: VertexAttributeKind::I32,
            },
            // specific clip attributes
            VertexAttribute {
                name: "aClipLocalPos",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipLocalRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipMode",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRect_TL",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRadii_TL",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRect_TR",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRadii_TR",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRect_BL",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRadii_BL",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRect_BR",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipRadii_BR",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const CLIP_BOX_SHADOW: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            // common clip attributes
            VertexAttribute {
                name: "aClipDeviceArea",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipOrigins",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aDevicePixelScale",
                count: 1,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aTransformIds",
                count: 2,
                kind: VertexAttributeKind::I32,
            },
            // specific clip attributes
            VertexAttribute {
                name: "aClipDataResourceAddress",
                count: 2,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aClipSrcRectSize",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aClipMode",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aStretchMode",
                count: 2,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aClipDestRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const GPU_CACHE_UPDATE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[
            VertexAttribute {
                name: "aPosition",
                count: 2,
                kind: VertexAttributeKind::U16Norm,
            },
            VertexAttribute {
                name: "aValue",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
        ],
        instance_attributes: &[],
    };

    pub const RESOLVE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[VertexAttribute {
            name: "aRect",
            count: 4,
            kind: VertexAttributeKind::F32,
        }],
    };

    pub const SVG_FILTER: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aFilterRenderTaskAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aFilterInput1TaskAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aFilterInput2TaskAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aFilterKind",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aFilterInputCount",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aFilterGenericInt",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aUnused",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aFilterExtraDataAddress",
                count: 2,
                kind: VertexAttributeKind::U16,
            },
        ],
    };

    pub const SVG_FILTER_NODE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aFilterTargetRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aFilterInput1ContentScaleAndOffset",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aFilterInput2ContentScaleAndOffset",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aFilterInput1TaskAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aFilterInput2TaskAddress",
                count: 1,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aFilterKind",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aFilterInputCount",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aFilterExtraDataAddress",
                count: 2,
                kind: VertexAttributeKind::U16,
            },
        ],
    };

    pub const MASK: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aData",
                count: 4,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aClipData",
                count: 4,
                kind: VertexAttributeKind::I32,
            },
        ],
    };

    pub const VECTOR_STENCIL: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aFromPosition",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aCtrlPosition",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aToPosition",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aFromNormal",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aCtrlNormal",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aToNormal",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aPathID",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aPad",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
        ],
    };

    pub const VECTOR_COVER: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aTargetRect",
                count: 4,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aStencilOrigin",
                count: 2,
                kind: VertexAttributeKind::I32,
            },
            VertexAttribute {
                name: "aSubpixel",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
            VertexAttribute {
                name: "aPad",
                count: 1,
                kind: VertexAttributeKind::U16,
            },
        ],
    };

    pub const COMPOSITE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aDeviceRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aDeviceClipRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aColor",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aParams",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aUvRect0",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aUvRect1",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aUvRect2",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aFlip",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aDeviceRoundedClipRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aDeviceRoundedClipRadii",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const CLEAR: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "aRect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "aColor",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
        ],
    };

    pub const COPY: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[VertexAttribute {
            name: "aPosition",
            count: 2,
            kind: VertexAttributeKind::U8Norm,
        }],
        instance_attributes: &[
            VertexAttribute {
                name: "a_src_rect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "a_dst_rect",
                count: 4,
                kind: VertexAttributeKind::F32,
            },
            VertexAttribute {
                name: "a_dst_texture_size",
                count: 2,
                kind: VertexAttributeKind::F32,
            },
        ],
    };
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum VertexArrayKind {
    Primitive,
    Blur,
    ClipRect,
    ClipBoxShadow,
    VectorStencil,
    VectorCover,
    Border,
    Scale,
    LineDecoration,
    FastLinearGradient,
    LinearGradient,
    RadialGradient,
    ConicGradient,
    Resolve,
    SvgFilter,
    SvgFilterNode,
    Composite,
    Clear,
    Copy,
    Mask,
}

pub struct VertexDataTexture<T> {
    texture: Option<Texture>,
    format: api::ImageFormat,
    _marker: PhantomData<T>,
}

impl<T> VertexDataTexture<T> {
    pub fn new(format: api::ImageFormat) -> Self {
        Self {
            texture: None,
            format,
            _marker: PhantomData,
        }
    }

    /// Returns a borrow of the GPU texture. Panics if it hasn't been initialized.
    pub fn texture(&self) -> &Texture {
        self.texture.as_ref().unwrap()
    }

    /// Returns an estimate of the GPU memory consumed by this VertexDataTexture.
    pub fn size_in_bytes(&self) -> usize {
        self.texture.as_ref().map_or(0, |t| t.size_in_bytes())
    }

    fn ensure_texture<D: GpuDevice<Texture = Texture>>(&mut self, device: &mut D, needed_height: i32) {
        let existing_height = self
            .texture
            .as_ref()
            .map_or(0, |t| t.get_dimensions().height);

        if needed_height > existing_height
            || needed_height + VERTEX_TEXTURE_EXTRA_ROWS < existing_height
        {
            if let Some(texture) = self.texture.take() {
                device.delete_texture(texture);
            }

            let texture = device.create_texture(
                api::ImageBufferKind::Texture2D,
                self.format,
                MAX_VERTEX_TEXTURE_WIDTH as i32,
                needed_height.max(2),
                TextureFilter::Nearest,
                None,
            );
            self.texture = Some(texture);
        }
    }

    pub fn update<'a>(
        &'a mut self,
        device: &mut Device,
        texture_uploader: &mut TextureUploader<'a>,
        data: &mut FrameVec<T>,
    ) {
        use super::data_texture_layout as layout;

        let texels_per_item = layout::texels_per_item::<T>();
        let items_per_row = layout::items_per_row(texels_per_item);

        // Ensure we always end up with a texture when leaving this method.
        let mut len = data.len();
        if len == 0 {
            if self.texture.is_some() {
                return;
            }
            data.reserve(items_per_row);
            len = items_per_row;
        } else {
            // Extend the data array to have enough capacity to upload at least
            // a multiple of the row size.  This ensures memory safety when the
            // array is passed to OpenGL to upload to the GPU.
            let extra = len % items_per_row;
            if extra != 0 {
                let padding = items_per_row - extra;
                data.reserve(padding);
                len += padding;
            }
        }

        let needed_height = (len / items_per_row) as i32;

        // Create a new texture if needed.
        //
        // These textures are generally very small, which is why we don't bother
        // with incremental updates and just re-upload every frame. For most pages
        // they're one row each, and on stress tests like css-francine they end up
        // in the 6-14 range. So we size the texture tightly to what we need (usually
        // 1), and shrink it if the waste would be more than `VERTEX_TEXTURE_EXTRA_ROWS`
        // rows. This helps with memory overhead, especially because there are several
        // instances of these textures per Renderer.
        self.ensure_texture(device, needed_height);

        // Note: the actual width can be larger than the logical one, with a few texels
        // of each row unused at the tail. This is needed because there is still hardware
        // (like Intel iGPUs) that prefers power-of-two sizes of textures ([1]).
        //
        // [1] https://software.intel.com/en-us/articles/opengl-performance-tips-power-of-two-textures-have-better-performance
        let logical_width = layout::logical_width(data.len(), texels_per_item, needed_height as usize);

        let rect = DeviceIntRect::from_size(
            DeviceIntSize::new(logical_width as i32, needed_height),
        );

        debug_assert!(len <= data.capacity(), "CPU copy will read out of bounds");
        texture_uploader.upload(
            device,
            self.texture(),
            rect,
            None,
            None,
            data.as_ptr(),
            len,
        );
    }

    pub fn deinit<D: GpuDevice<Texture = Texture>>(mut self, device: &mut D) {
        if let Some(t) = self.texture.take() {
            device.delete_texture(t);
        }
    }
}

pub struct VertexDataTextures {
    prim_header_f_texture: VertexDataTexture<PrimitiveHeaderF>,
    prim_header_i_texture: VertexDataTexture<PrimitiveHeaderI>,
    transforms_texture: VertexDataTexture<TransformData>,
    render_task_texture: VertexDataTexture<RenderTaskData>,
}

impl VertexDataTextures {
    pub fn new() -> Self {
        VertexDataTextures {
            prim_header_f_texture: VertexDataTexture::new(api::ImageFormat::RGBAF32),
            prim_header_i_texture: VertexDataTexture::new(api::ImageFormat::RGBAI32),
            transforms_texture: VertexDataTexture::new(api::ImageFormat::RGBAF32),
            render_task_texture: VertexDataTexture::new(api::ImageFormat::RGBAF32),
        }
    }

    pub fn update(&mut self, device: &mut Device, pbo_pool: &mut UploadPBOPool, frame: &mut Frame) {
        let mut texture_uploader = device.upload_texture(pbo_pool);
        self.prim_header_f_texture.update(
            device,
            &mut texture_uploader,
            &mut frame.prim_headers.headers_float,
        );
        self.prim_header_i_texture.update(
            device,
            &mut texture_uploader,
            &mut frame.prim_headers.headers_int,
        );
        self.transforms_texture
            .update(device, &mut texture_uploader, &mut frame.transform_palette);
        self.render_task_texture.update(
            device,
            &mut texture_uploader,
            &mut frame.render_tasks.task_data,
        );

        // Flush and drop the texture uploader now, so that
        // we can borrow the textures to bind them.
        texture_uploader.flush(device);

        device.bind_texture(
            super::TextureSampler::PrimitiveHeadersF,
            &self.prim_header_f_texture.texture(),
            Swizzle::default(),
        );
        device.bind_texture(
            super::TextureSampler::PrimitiveHeadersI,
            &self.prim_header_i_texture.texture(),
            Swizzle::default(),
        );
        device.bind_texture(
            super::TextureSampler::TransformPalette,
            &self.transforms_texture.texture(),
            Swizzle::default(),
        );
        device.bind_texture(
            super::TextureSampler::RenderTasks,
            &self.render_task_texture.texture(),
            Swizzle::default(),
        );
    }

    pub fn size_in_bytes(&self) -> usize {
        self.prim_header_f_texture.size_in_bytes()
            + self.prim_header_i_texture.size_in_bytes()
            + self.transforms_texture.size_in_bytes()
            + self.render_task_texture.size_in_bytes()
    }

    pub fn deinit(self, device: &mut Device) {
        self.transforms_texture.deinit(device);
        self.prim_header_f_texture.deinit(device);
        self.prim_header_i_texture.deinit(device);
        self.render_task_texture.deinit(device);
    }
}

pub(super) struct GlRendererVertexData {
    textures: Vec<VertexDataTextures>,
    current: usize,
}

#[cfg(feature = "wgpu_backend")]
#[allow(dead_code)]
pub struct WgpuRendererVertexData;

#[cfg_attr(feature = "wgpu_backend", allow(dead_code))]
pub(super) enum RendererVertexData {
    Gl(GlRendererVertexData),
    #[cfg(feature = "wgpu_backend")]
    Wgpu(WgpuRendererVertexData),
}

impl RendererVertexData {
    pub fn new_gl() -> Self {
        let mut textures = Vec::new();
        for _ in 0 .. super::VERTEX_DATA_TEXTURE_COUNT {
            textures.push(VertexDataTextures::new());
        }

        Self::Gl(GlRendererVertexData {
            textures,
            current: 0,
        })
    }

    fn gl(&self) -> &GlRendererVertexData {
        match self {
            Self::Gl(state) => state,
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => unreachable!("wgpu vertex data backend is not wired yet"),
        }
    }

    fn gl_mut(&mut self) -> &mut GlRendererVertexData {
        match self {
            Self::Gl(state) => state,
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => unreachable!("wgpu vertex data backend is not wired yet"),
        }
    }

    pub fn bind_frame_data(
        &mut self,
        device: &mut Device,
        pbo_pool: &mut UploadPBOPool,
        frame: &mut Frame,
    ) {
        let state = self.gl_mut();
        state.textures[state.current].update(device, pbo_pool, frame);
        state.current = (state.current + 1) % super::VERTEX_DATA_TEXTURE_COUNT;
    }

    pub fn deinit(self, device: &mut Device) {
        match self {
            Self::Gl(state) => {
                for textures in state.textures {
                    textures.deinit(device);
                }
            }
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => {}
        }
    }

    pub fn report_memory_to(&self, report: &mut MemoryReport) {
        for textures in &self.gl().textures {
            report.vertex_data_textures += textures.size_in_bytes();
        }
    }
}

pub struct RendererVAOs {
    prim_vao: VAO,
    blur_vao: VAO,
    clip_rect_vao: VAO,
    clip_box_shadow_vao: VAO,
    border_vao: VAO,
    line_vao: VAO,
    scale_vao: VAO,
    fast_linear_gradient_vao: VAO,
    linear_gradient_vao: VAO,
    radial_gradient_vao: VAO,
    conic_gradient_vao: VAO,
    resolve_vao: VAO,
    svg_filter_vao: VAO,
    svg_filter_node_vao: VAO,
    composite_vao: VAO,
    clear_vao: VAO,
    copy_vao: VAO,
    mask_vao: VAO,
}

impl RendererVAOs {
    pub fn new(device: &mut Device, indexed_quads: Option<NonZeroUsize>) -> Self {
        const QUAD_INDICES: [u16; 6] = [0, 1, 2, 2, 1, 3];
        const QUAD_VERTICES: [[u8; 2]; 4] = [[0, 0], [0xFF, 0], [0, 0xFF], [0xFF, 0xFF]];

        let instance_divisor = if indexed_quads.is_some() { 0 } else { 1 };
        let prim_vao = device.create_vao(&desc::PRIM_INSTANCES, instance_divisor);

        device.bind_vao(&prim_vao);
        match indexed_quads {
            Some(count) => {
                assert!(count.get() < u16::MAX as usize);
                let quad_indices = (0 .. count.get() as u16)
                    .flat_map(|instance| QUAD_INDICES.iter().map(move |&index| instance * 4 + index))
                    .collect::<Vec<_>>();
                device.update_vao_indices(&prim_vao, &quad_indices, VertexUsageHint::Static);
                let quad_vertices = (0 .. count.get() as u16)
                    .flat_map(|_| QUAD_VERTICES.iter().cloned())
                    .collect::<Vec<_>>();
                device.update_vao_main_vertices(&prim_vao, &quad_vertices, VertexUsageHint::Static);
            }
            None => {
                device.update_vao_indices(&prim_vao, &QUAD_INDICES, VertexUsageHint::Static);
                device.update_vao_main_vertices(&prim_vao, &QUAD_VERTICES, VertexUsageHint::Static);
            }
        }

        RendererVAOs {
            blur_vao: device.create_vao_with_new_instances(&desc::BLUR, &prim_vao),
            clip_rect_vao: device.create_vao_with_new_instances(&desc::CLIP_RECT, &prim_vao),
            clip_box_shadow_vao: device
                .create_vao_with_new_instances(&desc::CLIP_BOX_SHADOW, &prim_vao),
            border_vao: device.create_vao_with_new_instances(&desc::BORDER, &prim_vao),
            scale_vao: device.create_vao_with_new_instances(&desc::SCALE, &prim_vao),
            line_vao: device.create_vao_with_new_instances(&desc::LINE, &prim_vao),
            fast_linear_gradient_vao: device.create_vao_with_new_instances(&desc::FAST_LINEAR_GRADIENT, &prim_vao),
            linear_gradient_vao: device.create_vao_with_new_instances(&desc::LINEAR_GRADIENT, &prim_vao),
            radial_gradient_vao: device.create_vao_with_new_instances(&desc::RADIAL_GRADIENT, &prim_vao),
            conic_gradient_vao: device.create_vao_with_new_instances(&desc::CONIC_GRADIENT, &prim_vao),
            resolve_vao: device.create_vao_with_new_instances(&desc::RESOLVE, &prim_vao),
            svg_filter_vao: device.create_vao_with_new_instances(&desc::SVG_FILTER, &prim_vao),
            svg_filter_node_vao: device.create_vao_with_new_instances(&desc::SVG_FILTER_NODE, &prim_vao),
            composite_vao: device.create_vao_with_new_instances(&desc::COMPOSITE, &prim_vao),
            clear_vao: device.create_vao_with_new_instances(&desc::CLEAR, &prim_vao),
            copy_vao: device.create_vao_with_new_instances(&desc::COPY, &prim_vao),
            mask_vao: device.create_vao_with_new_instances(&desc::MASK, &prim_vao),
            prim_vao,
        }
    }

    pub fn deinit(self, device: &mut Device) {
        device.delete_vao(self.prim_vao);
        device.delete_vao(self.resolve_vao);
        device.delete_vao(self.clip_rect_vao);
        device.delete_vao(self.clip_box_shadow_vao);
        device.delete_vao(self.fast_linear_gradient_vao);
        device.delete_vao(self.linear_gradient_vao);
        device.delete_vao(self.radial_gradient_vao);
        device.delete_vao(self.conic_gradient_vao);
        device.delete_vao(self.blur_vao);
        device.delete_vao(self.line_vao);
        device.delete_vao(self.border_vao);
        device.delete_vao(self.scale_vao);
        device.delete_vao(self.svg_filter_vao);
        device.delete_vao(self.svg_filter_node_vao);
        device.delete_vao(self.composite_vao);
        device.delete_vao(self.clear_vao);
        device.delete_vao(self.copy_vao);
        device.delete_vao(self.mask_vao);
    }
}

pub(super) struct GlRendererVaos {
    vaos: RendererVAOs,
}

#[cfg(feature = "wgpu_backend")]
#[allow(dead_code)]
pub struct WgpuRendererVaos;

#[cfg_attr(feature = "wgpu_backend", allow(dead_code))]
pub(super) enum RendererVaoState {
    Gl(GlRendererVaos),
    #[cfg(feature = "wgpu_backend")]
    Wgpu(WgpuRendererVaos),
}

impl RendererVaoState {
    pub fn new_gl(device: &mut Device, indexed_quads: Option<NonZeroUsize>) -> Self {
        Self::Gl(GlRendererVaos {
            vaos: RendererVAOs::new(device, indexed_quads),
        })
    }

    fn gl(&self) -> &GlRendererVaos {
        match self {
            Self::Gl(state) => state,
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => unreachable!("wgpu vao state is not wired yet"),
        }
    }

    pub fn vao(&self, kind: VertexArrayKind) -> &VAO {
        &self.gl().vaos[kind]
    }

    pub fn draw_instanced_batch<T: Clone>(
        &self,
        device: &mut Device,
        data: &[T],
        kind: VertexArrayKind,
        enable_instancing: bool,
        chunk_size: usize,
    ) -> usize {
        let vao = self.vao(kind);
        device.bind_vao(vao);

        let mut draw_calls = 0;
        for chunk in data.chunks(chunk_size) {
            if enable_instancing {
                device.update_vao_instances(vao, chunk, VertexUsageHint::Stream, None);
                device.draw_indexed_triangles_instanced_u16(6, chunk.len() as i32);
            } else {
                device.update_vao_instances(
                    vao,
                    chunk,
                    VertexUsageHint::Stream,
                    NonZeroUsize::new(4),
                );
                device.draw_indexed_triangles(6 * chunk.len() as i32);
            }
            draw_calls += 1;
        }

        draw_calls
    }

    pub fn deinit(self, device: &mut Device) {
        match self {
            Self::Gl(state) => state.vaos.deinit(device),
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => {}
        }
    }
}

impl ops::Index<VertexArrayKind> for RendererVAOs {
    type Output = VAO;
    fn index(&self, kind: VertexArrayKind) -> &VAO {
        match kind {
            VertexArrayKind::Primitive => &self.prim_vao,
            VertexArrayKind::ClipRect => &self.clip_rect_vao,
            VertexArrayKind::ClipBoxShadow => &self.clip_box_shadow_vao,
            VertexArrayKind::Blur => &self.blur_vao,
            VertexArrayKind::VectorStencil | VertexArrayKind::VectorCover => unreachable!(),
            VertexArrayKind::Border => &self.border_vao,
            VertexArrayKind::Scale => &self.scale_vao,
            VertexArrayKind::LineDecoration => &self.line_vao,
            VertexArrayKind::FastLinearGradient => &self.fast_linear_gradient_vao,
            VertexArrayKind::LinearGradient => &self.linear_gradient_vao,
            VertexArrayKind::RadialGradient => &self.radial_gradient_vao,
            VertexArrayKind::ConicGradient => &self.conic_gradient_vao,
            VertexArrayKind::Resolve => &self.resolve_vao,
            VertexArrayKind::SvgFilter => &self.svg_filter_vao,
            VertexArrayKind::SvgFilterNode => &self.svg_filter_node_vao,
            VertexArrayKind::Composite => &self.composite_vao,
            VertexArrayKind::Clear => &self.clear_vao,
            VertexArrayKind::Copy => &self.copy_vao,
            VertexArrayKind::Mask => &self.mask_vao,
        }
    }
}
