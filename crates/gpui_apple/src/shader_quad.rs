use anyhow::{Context as _, Result, anyhow};
use collections::FxHashMap;
use gpui::{
    Bounds, DevicePixels, MAX_SHADER_TEXTURES, PaintShader, PlatformTextures, Size, TextureFormat,
    TextureId,
};
use gpui_shader::{ShaderModule, ShaderQuadInstance, msl};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBlitCommandEncoder, MTLBuffer, MTLCommandBuffer,
    MTLCommandEncoder, MTLCompileOptions, MTLDevice, MTLLibrary, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLResourceOptions, MTLSize, MTLStorageMode, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage,
};
use parking_lot::Mutex;
use std::{mem, ptr, sync::Arc};

pub struct MetalTextures {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    state: Mutex<TexturesState>,
}

#[derive(Default)]
struct TexturesState {
    next_id: u64,
    textures: FxHashMap<TextureId, Texture>,
    pending_uploads: Vec<PendingUpload>,
}

struct Texture {
    metal_texture: Retained<ProtocolObject<dyn MTLTexture>>,
    size: Size<DevicePixels>,
    format: TextureFormat,
}

struct PendingUpload {
    staging: Retained<ProtocolObject<dyn MTLBuffer>>,
    bytes_per_row: usize,
    rect: Bounds<DevicePixels>,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
}

// SAFETY: Metal textures and buffers may be used from any thread; the mutex serializes the bookkeeping.
unsafe impl Send for MetalTextures {}
// SAFETY: See the `Send` impl above.
unsafe impl Sync for MetalTextures {}

impl MetalTextures {
    pub fn new(device: Retained<ProtocolObject<dyn MTLDevice>>) -> Self {
        Self {
            device,
            state: Mutex::default(),
        }
    }

    fn metal_texture(&self, id: TextureId) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
        self.state
            .lock()
            .textures
            .get(&id)
            .map(|texture| texture.metal_texture.clone())
    }

    pub fn encode_uploads(
        &self,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    ) -> Result<()> {
        let uploads = mem::take(&mut self.state.lock().pending_uploads);
        if uploads.is_empty() {
            return Ok(());
        }
        let encoder = command_buffer
            .blitCommandEncoder()
            .context("creating texture upload encoder")?;
        for upload in &uploads {
            let width = upload.rect.size.width.0 as usize;
            let height = upload.rect.size.height.0 as usize;
            // SAFETY: The staging buffer holds `height` rows of `bytes_per_row`, and `create` and
            // `update` checked that the rect lies within the destination texture.
            unsafe {
                encoder.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                    &upload.staging,
                    0,
                    upload.bytes_per_row,
                    upload.bytes_per_row * height,
                    MTLSize {
                        width,
                        height,
                        depth: 1,
                    },
                    &upload.texture,
                    0,
                    0,
                    MTLOrigin {
                        x: upload.rect.origin.x.0 as usize,
                        y: upload.rect.origin.y.0 as usize,
                        z: 0,
                    },
                );
            }
        }
        encoder.endEncoding();
        Ok(())
    }
}

impl PlatformTextures for MetalTextures {
    fn create(&self, size: Size<DevicePixels>, format: TextureFormat) -> Result<TextureId> {
        anyhow::ensure!(
            size.width.0 > 0
                && size.height.0 > 0
                && size.width.0 <= 16384
                && size.height.0 <= 16384,
            "texture size {size:?} is outside 1..=16384"
        );
        let descriptor = MTLTextureDescriptor::new();
        // SAFETY: Both dimensions were checked against the Metal texture size limit above.
        unsafe {
            descriptor.setWidth(size.width.0 as usize);
            descriptor.setHeight(size.height.0 as usize);
        }
        descriptor.setPixelFormat(pixel_format(format));
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        descriptor.setStorageMode(MTLStorageMode::Private);
        let metal_texture = self
            .device
            .newTextureWithDescriptor(&descriptor)
            .context("allocating shader texture")?;

        let mut state = self.state.lock();
        let id = TextureId(state.next_id);
        state.next_id += 1;
        state.textures.insert(
            id,
            Texture {
                metal_texture,
                size,
                format,
            },
        );
        Ok(id)
    }

    fn update(
        &self,
        id: TextureId,
        rect: Bounds<DevicePixels>,
        bytes: &[u8],
        bytes_per_row: usize,
    ) -> Result<()> {
        let (texture, row_len) = {
            let state = self.state.lock();
            let texture = state
                .textures
                .get(&id)
                .with_context(|| format!("unknown texture {id:?}"))?;
            anyhow::ensure!(
                rect.origin.x.0 >= 0
                    && rect.origin.y.0 >= 0
                    && rect.size.width.0 >= 0
                    && rect.size.height.0 >= 0
                    && rect.origin.x.0 + rect.size.width.0 <= texture.size.width.0
                    && rect.origin.y.0 + rect.size.height.0 <= texture.size.height.0,
                "update rect {rect:?} is outside texture of size {:?}",
                texture.size
            );
            (
                texture.metal_texture.clone(),
                rect.size.width.0 as usize * texture.format.bytes_per_pixel(),
            )
        };
        let height = rect.size.height.0 as usize;
        if row_len == 0 || height == 0 {
            return Ok(());
        }
        anyhow::ensure!(
            bytes_per_row >= row_len,
            "bytes_per_row {bytes_per_row} is less than one row of {row_len} bytes"
        );
        let required = bytes_per_row * (height - 1) + row_len;
        anyhow::ensure!(
            bytes.len() >= required,
            "{} bytes given, the rect needs {required}",
            bytes.len()
        );

        let staging = self
            .device
            .newBufferWithLength_options(row_len * height, MTLResourceOptions::StorageModeShared)
            .context("allocating texture upload buffer")?;
        // SAFETY: The shared buffer is `row_len * height` bytes and not yet visible to the GPU,
        // and each source row was bounds-checked above.
        unsafe {
            let destination = staging.contents().as_ptr().cast::<u8>();
            for row in 0..height {
                ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(row * bytes_per_row),
                    destination.add(row * row_len),
                    row_len,
                );
            }
        }
        self.state.lock().pending_uploads.push(PendingUpload {
            staging,
            bytes_per_row: row_len,
            rect,
            texture,
        });
        Ok(())
    }

    fn remove(&self, id: TextureId) {
        self.state.lock().textures.remove(&id);
    }

    fn generation(&self) -> u64 {
        0
    }
}

fn pixel_format(format: TextureFormat) -> MTLPixelFormat {
    match format {
        TextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        TextureFormat::Rgba16Unorm => MTLPixelFormat::RGBA16Unorm,
    }
}

struct Pipeline {
    state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    params_size: usize,
}

pub struct ShaderQuads {
    textures: Arc<MetalTextures>,
    pipelines: FxHashMap<u64, Option<Pipeline>>,
    empty_texture: Retained<ProtocolObject<dyn MTLTexture>>,
    reported_params: FxHashMap<u64, usize>,
}

impl ShaderQuads {
    pub fn new(device: &Retained<ProtocolObject<dyn MTLDevice>>) -> Result<Self> {
        let descriptor = MTLTextureDescriptor::new();
        // SAFETY: A 1x1 texture is within every device's limits.
        unsafe {
            descriptor.setWidth(1);
            descriptor.setHeight(1);
        }
        descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        let empty_texture = device
            .newTextureWithDescriptor(&descriptor)
            .context("allocating empty shader texture")?;
        let zero = [0u8; 4];
        // SAFETY: The region covers the single texel and `zero` holds one 4-byte texel.
        unsafe {
            empty_texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                MTLRegion {
                    origin: MTLOrigin { x: 0, y: 0, z: 0 },
                    size: MTLSize {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                },
                0,
                ptr::NonNull::from(&zero).cast(),
                4,
            );
        }
        Ok(Self {
            textures: Arc::new(MetalTextures::new(device.clone())),
            pipelines: FxHashMap::default(),
            empty_texture,
            reported_params: FxHashMap::default(),
        })
    }

    pub fn textures(&self) -> &Arc<MetalTextures> {
        &self.textures
    }

    pub fn draw(
        &mut self,
        shaders: &[PaintShader],
        device: &ProtocolObject<dyn MTLDevice>,
        pixel_format: MTLPixelFormat,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) {
        for shader in shaders {
            let program = &shader.program;
            let pipeline = self.pipelines.entry(program.id()).or_insert_with(|| {
                build_pipeline(device, program.source(), pixel_format)
                    .map_err(|error| log::error!("compiling shader quad: {error:#}"))
                    .ok()
            });
            let Some(pipeline) = pipeline else {
                continue;
            };
            if shader.params.len() != pipeline.params_size {
                if self
                    .reported_params
                    .insert(program.id(), shader.params.len())
                    != Some(shader.params.len())
                {
                    log::error!(
                        "shader quad params are {} bytes, its ShaderParams is {}",
                        shader.params.len(),
                        pipeline.params_size
                    );
                }
                continue;
            }

            let mask = &shader.content_mask;
            let instance = ShaderQuadInstance {
                viewport_size: [viewport_size.width.0 as f32, viewport_size.height.0 as f32],
                opacity: shader.opacity,
                pad: 0.,
                bounds: [
                    shader.bounds.origin.x.0,
                    shader.bounds.origin.y.0,
                    shader.bounds.size.width.0,
                    shader.bounds.size.height.0,
                ],
                content_mask_bounds: [
                    mask.bounds.origin.x.0,
                    mask.bounds.origin.y.0,
                    mask.bounds.size.width.0,
                    mask.bounds.size.height.0,
                ],
                content_mask_corner_radii: [
                    mask.corner_radii.top_left.0,
                    mask.corner_radii.top_right.0,
                    mask.corner_radii.bottom_right.0,
                    mask.corner_radii.bottom_left.0,
                ],
                content_mask_corner_smoothing: mask.corner_smoothing,
                content_mask_pad: 0.,
            };
            let textures: [Retained<ProtocolObject<dyn MTLTexture>>; MAX_SHADER_TEXTURES] =
                std::array::from_fn(|index| {
                    shader.textures[index]
                        .and_then(|id| self.textures.metal_texture(id))
                        .unwrap_or_else(|| self.empty_texture.clone())
                });

            command_encoder.setRenderPipelineState(&pipeline.state);
            // SAFETY: The instance and params bytes are copied by Metal and match the translated
            // shader's buffer slots and sizes; every texture slot is bound.
            unsafe {
                command_encoder.setVertexBytes_length_atIndex(
                    ptr::NonNull::from(&instance).cast(),
                    mem::size_of_val(&instance),
                    msl::INSTANCE_BUFFER as usize,
                );
                command_encoder.setFragmentBytes_length_atIndex(
                    ptr::NonNull::from(&instance).cast(),
                    mem::size_of_val(&instance),
                    msl::INSTANCE_BUFFER as usize,
                );
                command_encoder.setFragmentBytes_length_atIndex(
                    ptr::NonNull::new_unchecked(shader.params.as_ptr().cast_mut()).cast(),
                    shader.params.len(),
                    msl::PARAMS_BUFFER as usize,
                );
                for (index, texture) in textures.iter().enumerate() {
                    command_encoder.setFragmentTexture_atIndex(Some(texture), index);
                }
                command_encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::TriangleStrip,
                    0,
                    4,
                );
            }
        }
    }
}

fn build_pipeline(
    device: &ProtocolObject<dyn MTLDevice>,
    source: &str,
    pixel_format: MTLPixelFormat,
) -> Result<Pipeline> {
    let module = ShaderModule::new(source)?;
    let msl = module.to_msl()?;
    let options = MTLCompileOptions::new();
    #[allow(deprecated)]
    options.setFastMathEnabled(false);
    let library = device
        .newLibraryWithSource_options_error(&NSString::from_str(&msl.source), Some(&options))
        .map_err(|error| anyhow!("compiling MSL: {}", error.localizedDescription()))?;
    let vertex_fn = library
        .newFunctionWithName(&NSString::from_str(&msl.vertex_entry))
        .context("locating shader quad vertex function")?;
    let fragment_fn = library
        .newFunctionWithName(&NSString::from_str(&msl.fragment_entry))
        .context("locating shader quad fragment function")?;

    let descriptor = MTLRenderPipelineDescriptor::new();
    descriptor.setLabel(Some(&NSString::from_str("shader_quad")));
    descriptor.setVertexFunction(Some(&vertex_fn));
    descriptor.setFragmentFunction(Some(&fragment_fn));
    // SAFETY: Index zero is within the pipeline's color attachment array.
    let color_attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    color_attachment.setPixelFormat(pixel_format);
    color_attachment.setBlendingEnabled(true);
    color_attachment.setRgbBlendOperation(MTLBlendOperation::Add);
    color_attachment.setAlphaBlendOperation(MTLBlendOperation::Add);
    color_attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
    color_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
    color_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    let state = device
        .newRenderPipelineStateWithDescriptor_error(&descriptor)
        .map_err(|error| {
            anyhow!(
                "creating shader quad pipeline: {}",
                error.localizedDescription()
            )
        })?;
    Ok(Pipeline {
        state,
        params_size: module.params_size() as usize,
    })
}

#[cfg(test)]
mod tests {
    use crate::metal_renderer::{InstanceBufferPool, MetalRenderer};
    use gpui::{
        Bounds, ContentMask, Corners, DevicePixels, PaintShader, PlatformTextures, Quad,
        ScaledPixels, Scene, ShaderProgram, TextureFormat, point, rgb, size,
    };
    use parking_lot::Mutex;
    use std::sync::Arc;

    const COPY: &str = "fn shade(input: ShadeInput) -> vec4<f32> {\n\
        return textureLoad(t0, vec2<i32>(input.local), 0);\n\
    }";
    const TINT: &str = "struct ShaderParams { color: vec4<f32> }\n\
        fn shade(input: ShadeInput) -> vec4<f32> { return params.color; }";

    fn renderer() -> MetalRenderer {
        MetalRenderer::new_headless(Arc::new(Mutex::new(InstanceBufferPool::default()))).unwrap()
    }

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds::new(
            point(ScaledPixels(x), ScaledPixels(y)),
            size(ScaledPixels(width), ScaledPixels(height)),
        )
    }

    fn shader(
        program: &ShaderProgram,
        area: Bounds<ScaledPixels>,
        params: &[u8],
        textures: &[gpui::TextureId],
    ) -> PaintShader {
        let mut slots = [None; gpui::MAX_SHADER_TEXTURES];
        for (slot, texture) in slots.iter_mut().zip(textures) {
            *slot = Some(*texture);
        }
        PaintShader {
            order: 0,
            bounds: area,
            content_mask: ContentMask {
                bounds: area,
                ..Default::default()
            },
            opacity: 1.,
            program: program.clone(),
            params: params.into(),
            textures: slots,
        }
    }

    fn color_params(color: [f32; 4]) -> Vec<u8> {
        color.iter().flat_map(|value| value.to_ne_bytes()).collect()
    }

    fn render(renderer: &mut MetalRenderer, scene: &mut Scene, side: i32) -> image::RgbaImage {
        scene.finish();
        renderer
            .render_scene_to_image(scene, size(DevicePixels(side), DevicePixels(side)))
            .unwrap()
    }

    #[test]
    fn srgb_texels_round_trip_exactly() {
        let mut renderer = renderer();
        let textures = renderer.textures().clone();
        let texture = textures
            .create(
                size(DevicePixels(16), DevicePixels(16)),
                TextureFormat::Bgra8Unorm,
            )
            .unwrap();
        let mut bytes = Vec::new();
        for index in 0..256u32 {
            let value = index as u8;
            bytes.extend_from_slice(&[value, 255 - value, value.wrapping_mul(7), 255]);
        }
        textures
            .update(
                texture,
                Bounds::new(
                    point(DevicePixels(0), DevicePixels(0)),
                    size(DevicePixels(16), DevicePixels(16)),
                ),
                &bytes,
                64,
            )
            .unwrap();

        let program = ShaderProgram::new(COPY);
        let mut scene = Scene::default();
        scene.insert_primitive(shader(
            &program,
            bounds(0., 0., 16., 16.),
            &color_params([0.; 4]),
            &[texture],
        ));
        let image = render(&mut renderer, &mut scene, 16);
        for (index, pixel) in image.pixels().enumerate() {
            let value = index as u8;
            assert_eq!(
                pixel.0,
                [value.wrapping_mul(7), 255 - value, value, 255],
                "texel {index}"
            );
        }
    }

    #[test]
    fn partial_update_reads_strided_source() {
        let mut renderer = renderer();
        let textures = renderer.textures().clone();
        let texture = textures
            .create(
                size(DevicePixels(8), DevicePixels(8)),
                TextureFormat::Bgra8Unorm,
            )
            .unwrap();
        textures
            .update(
                texture,
                Bounds::new(
                    point(DevicePixels(0), DevicePixels(0)),
                    size(DevicePixels(8), DevicePixels(8)),
                ),
                &[0, 0, 0, 255].repeat(64),
                32,
            )
            .unwrap();
        let mut source = vec![0u8; 10 * 10 * 4];
        for y in 2..5 {
            for x in 3..5 {
                let index = (y * 10 + x) * 4;
                source[index..index + 4].copy_from_slice(&[0, 0, 255, 255]);
            }
        }
        textures
            .update(
                texture,
                Bounds::new(
                    point(DevicePixels(1), DevicePixels(4)),
                    size(DevicePixels(2), DevicePixels(3)),
                ),
                &source[(2 * 10 + 3) * 4..],
                40,
            )
            .unwrap();

        let program = ShaderProgram::new(COPY);
        let mut scene = Scene::default();
        scene.insert_primitive(shader(
            &program,
            bounds(0., 0., 8., 8.),
            &color_params([0.; 4]),
            &[texture],
        ));
        let image = render(&mut renderer, &mut scene, 8);
        for (x, y, pixel) in image.enumerate_pixels() {
            let inside = (1..3).contains(&x) && (4..7).contains(&y);
            let expected = if inside {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(pixel.0, expected, "pixel {x},{y}");
        }
    }

    #[test]
    fn follows_draw_order_mask_and_opacity() {
        let mut renderer = renderer();
        let program = ShaderProgram::new(TINT);
        let mut scene = Scene::default();
        scene.insert_primitive(Quad {
            bounds: bounds(0., 0., 32., 32.),
            content_mask: ContentMask {
                bounds: bounds(0., 0., 32., 32.),
                ..Default::default()
            },
            background: rgb(0x0000ff).into(),
            ..Default::default()
        });
        let mut rounded = shader(
            &program,
            bounds(0., 0., 32., 32.),
            &color_params([1., 0., 0., 1.]),
            &[],
        );
        rounded.content_mask.corner_radii = Corners::all(ScaledPixels(12.));
        scene.insert_primitive(rounded);
        scene.insert_primitive(Quad {
            bounds: bounds(24., 24., 8., 8.),
            content_mask: ContentMask {
                bounds: bounds(24., 24., 8., 8.),
                ..Default::default()
            },
            background: rgb(0x00ff00).into(),
            ..Default::default()
        });
        let mut faded = shader(
            &program,
            bounds(16., 0., 8., 8.),
            &color_params([1., 1., 1., 1.]),
            &[],
        );
        faded.opacity = 0.5;
        scene.insert_primitive(faded);
        let image = render(&mut renderer, &mut scene, 32);

        assert_eq!(image.get_pixel(0, 0).0, [0, 0, 255, 255]);
        assert_eq!(image.get_pixel(16, 16).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(28, 28).0, [0, 255, 0, 255]);
        let [red, green, blue, _] = image.get_pixel(20, 4).0;
        assert!(
            red == 255 && green.abs_diff(128) <= 1 && blue.abs_diff(128) <= 1,
            "{red} {green} {blue}"
        );
    }

    #[test]
    fn mismatched_params_skip_the_quad() {
        let mut renderer = renderer();
        let program = ShaderProgram::new(TINT);
        let mut scene = Scene::default();
        scene.insert_primitive(shader(&program, bounds(0., 0., 4., 4.), &[0; 8], &[]));
        let image = render(&mut renderer, &mut scene, 4);
        assert!(image.pixels().all(|pixel| pixel.0 == [0, 0, 0, 255]));
    }
}
