use crate::WgpuContext;
use crate::wgpu_renderer::DEPTH_FORMAT;
use anyhow::{Context as _, Result};
use collections::FxHashMap;
use gpui::{
    Bounds, DevicePixels, MAX_SHADER_PARAMS, MAX_SHADER_TEXTURES, PaintShader, PlatformTextures,
    Size, TextureFormat, TextureId,
};
use gpui_shader::{ShaderModule, ShaderQuadInstance, TEXTURE_COUNT};
use parking_lot::Mutex;
use std::{borrow::Cow, mem, num::NonZeroU64, ops::Range, sync::Arc};

pub struct WgpuTextures(Mutex<TexturesState>);

struct TexturesState {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    color_format: wgpu::TextureFormat,
    next_id: u64,
    generation: u64,
    textures: FxHashMap<TextureId, Texture>,
}

struct Texture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: Size<DevicePixels>,
    format: TextureFormat,
}

impl WgpuTextures {
    pub fn from_context(context: &WgpuContext) -> Self {
        Self(Mutex::new(TexturesState {
            device: context.device.clone(),
            queue: context.queue.clone(),
            color_format: context.color_texture_format(),
            next_id: 0,
            generation: 0,
            textures: FxHashMap::default(),
        }))
    }

    pub fn handle_device_lost(&self, context: &WgpuContext) {
        let mut state = self.0.lock();
        state.device = context.device.clone();
        state.queue = context.queue.clone();
        state.color_format = context.color_texture_format();
        state.textures.clear();
        state.generation += 1;
    }

    fn view(&self, id: TextureId) -> Option<wgpu::TextureView> {
        self.0
            .lock()
            .textures
            .get(&id)
            .map(|texture| texture.view.clone())
    }
}

impl TexturesState {
    fn wgpu_format(&self, format: TextureFormat) -> wgpu::TextureFormat {
        match format {
            TextureFormat::Bgra8Unorm => self.color_format,
            TextureFormat::Rgba16Unorm => wgpu::TextureFormat::Rgba16Unorm,
        }
    }
}

impl PlatformTextures for WgpuTextures {
    fn create(&self, size: Size<DevicePixels>, format: TextureFormat) -> Result<TextureId> {
        let mut state = self.0.lock();
        let max = state.device.limits().max_texture_dimension_2d as i32;
        anyhow::ensure!(
            size.width.0 > 0 && size.height.0 > 0 && size.width.0 <= max && size.height.0 <= max,
            "texture size {size:?} is outside 1..={max}"
        );
        anyhow::ensure!(
            format != TextureFormat::Rgba16Unorm
                || state
                    .device
                    .features()
                    .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
            "this GPU does not support Rgba16Unorm textures"
        );
        let texture = create_texture(
            &state.device,
            size.width.0 as u32,
            size.height.0 as u32,
            state.wgpu_format(format),
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let id = TextureId(state.next_id);
        state.next_id += 1;
        state.textures.insert(
            id,
            Texture {
                texture,
                view,
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
        let state = self.0.lock();
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
        let row_len = rect.size.width.0 as usize * texture.format.bytes_per_pixel();
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

        let (data, stride) = if texture.texture.format() == wgpu::TextureFormat::Rgba8Unorm {
            let mut data = Vec::with_capacity(row_len * height);
            for row in 0..height {
                let start = row * bytes_per_row;
                data.extend_from_slice(&bytes[start..start + row_len]);
            }
            for pixel in data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            (Cow::Owned(data), row_len)
        } else {
            (Cow::Borrowed(&bytes[..required]), bytes_per_row)
        };
        state.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.origin.x.0 as u32,
                    y: rect.origin.y.0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride as u32),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: rect.size.width.0 as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    fn remove(&self, id: TextureId) {
        self.0.lock().textures.remove(&id);
    }

    fn generation(&self) -> u64 {
        self.0.lock().generation
    }
}

fn create_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shader_texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    params_size: usize,
}

struct PreparedQuad {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

pub(crate) struct ShaderQuads {
    textures: Arc<WgpuTextures>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    target_format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    empty_view: wgpu::TextureView,
    slot_size: u64,
    uniforms: Option<wgpu::Buffer>,
    pipelines: FxHashMap<u64, Option<Pipeline>>,
    reported_params: FxHashMap<u64, usize>,
    prepared: Vec<Option<PreparedQuad>>,
}

impl ShaderQuads {
    pub(crate) fn new(
        textures: Arc<WgpuTextures>,
        device: &Arc<wgpu::Device>,
        queue: &Arc<wgpu::Queue>,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let uniform = |binding, size: usize| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(size as u64),
            },
            count: None,
        };
        let mut entries = vec![
            uniform(0, mem::size_of::<ShaderQuadInstance>()),
            uniform(1, 0),
        ];
        entries.extend(
            (0..TEXTURE_COUNT as u32).map(|index| wgpu::BindGroupLayoutEntry {
                binding: 2 + index,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }),
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shader_quad_layout"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shader_quad_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let empty = create_texture(device, 1, 1, wgpu::TextureFormat::Rgba8Unorm);
        queue.write_texture(
            empty.as_image_copy(),
            &[0; 4],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        Self {
            textures,
            device: device.clone(),
            queue: queue.clone(),
            target_format,
            layout,
            pipeline_layout,
            empty_view: empty.create_view(&wgpu::TextureViewDescriptor::default()),
            slot_size: (MAX_SHADER_PARAMS as u64).next_multiple_of(alignment),
            uniforms: None,
            pipelines: FxHashMap::default(),
            reported_params: FxHashMap::default(),
            prepared: Vec::new(),
        }
    }

    pub(crate) fn prepare(&mut self, shaders: &[PaintShader], viewport_size: Size<DevicePixels>) {
        self.prepared.clear();
        if shaders.is_empty() {
            return;
        }
        let required = self.slot_size * 2 * shaders.len() as u64;
        if self
            .uniforms
            .as_ref()
            .is_none_or(|buffer| buffer.size() < required)
        {
            self.uniforms = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shader_quad_uniforms"),
                size: required.next_power_of_two(),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let Some(uniforms) = self.uniforms.clone() else {
            return;
        };

        for (index, shader) in shaders.iter().enumerate() {
            let program = &shader.program;
            let pipeline = self.pipelines.entry(program.id()).or_insert_with(|| {
                build_pipeline(
                    &self.device,
                    &self.pipeline_layout,
                    self.target_format,
                    program.source(),
                )
                .map_err(|error| log::error!("compiling shader quad: {error:#}"))
                .ok()
            });
            let Some(pipeline) = pipeline else {
                self.prepared.push(None);
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
                self.prepared.push(None);
                continue;
            }

            let instance_offset = self.slot_size * 2 * index as u64;
            let params_offset = instance_offset + self.slot_size;
            self.queue.write_buffer(
                &uniforms,
                instance_offset,
                &instance_bytes(shader, viewport_size),
            );
            self.queue
                .write_buffer(&uniforms, params_offset, &shader.params);

            let views: [wgpu::TextureView; MAX_SHADER_TEXTURES] = std::array::from_fn(|slot| {
                shader.textures[slot]
                    .and_then(|id| self.textures.view(id))
                    .unwrap_or_else(|| self.empty_view.clone())
            });
            let buffer_entry = |binding, offset, size: usize| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniforms,
                    offset,
                    size: NonZeroU64::new(size as u64),
                }),
            };
            let mut entries = vec![
                buffer_entry(0, instance_offset, mem::size_of::<ShaderQuadInstance>()),
                buffer_entry(1, params_offset, pipeline.params_size),
            ];
            entries.extend(
                views
                    .iter()
                    .enumerate()
                    .map(|(slot, view)| wgpu::BindGroupEntry {
                        binding: 2 + slot as u32,
                        resource: wgpu::BindingResource::TextureView(view),
                    }),
            );
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shader_quad_bind_group"),
                layout: &self.layout,
                entries: &entries,
            });
            self.prepared.push(Some(PreparedQuad {
                pipeline: pipeline.pipeline.clone(),
                bind_group,
            }));
        }
    }

    pub(crate) fn draw(&self, range: Range<usize>, pass: &mut wgpu::RenderPass<'_>) {
        for quad in self.prepared[range].iter().flatten() {
            pass.set_pipeline(&quad.pipeline);
            pass.set_bind_group(0, &quad.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
    }
}

fn instance_bytes(shader: &PaintShader, viewport_size: Size<DevicePixels>) -> Vec<u8> {
    let mask = &shader.content_mask;
    let values = [
        viewport_size.width.0 as f32,
        viewport_size.height.0 as f32,
        shader.opacity,
        0.,
        shader.bounds.origin.x.0,
        shader.bounds.origin.y.0,
        shader.bounds.size.width.0,
        shader.bounds.size.height.0,
        mask.bounds.origin.x.0,
        mask.bounds.origin.y.0,
        mask.bounds.size.width.0,
        mask.bounds.size.height.0,
        mask.corner_radii.top_left.0,
        mask.corner_radii.top_right.0,
        mask.corner_radii.bottom_right.0,
        mask.corner_radii.bottom_left.0,
        mask.corner_smoothing,
        0.,
    ];
    const _: () = assert!(mem::size_of::<ShaderQuadInstance>() == 18 * 4);
    values
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect()
}

fn build_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    source: &str,
) -> Result<Pipeline> {
    let module = ShaderModule::new(source)?;
    let params_size = module.params_size() as usize;
    anyhow::ensure!(
        params_size <= MAX_SHADER_PARAMS,
        "ShaderParams is {params_size} bytes, above the maximum of {MAX_SHADER_PARAMS}"
    );
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("shader_quad"),
        source: wgpu::ShaderSource::Wgsl(module.wgsl().into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("shader_quad"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some(gpui_shader::VERTEX_ENTRY),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some(gpui_shader::FRAGMENT_ENTRY),
            // `shade` returns straight alpha, so this stays ALPHA_BLENDING on premultiplied surfaces.
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    #[cfg(not(target_family = "wasm"))]
    if let Some(error) = gpui::block_on(scope.pop()) {
        anyhow::bail!("{error}");
    }
    #[cfg(target_family = "wasm")]
    drop(scope.pop());
    Ok(Pipeline {
        pipeline,
        params_size,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use crate::WgpuHeadlessRenderer;
    use gpui::{
        Bounds, ContentMask, Corners, DevicePixels, PaintShader, PlatformHeadlessRenderer,
        PlatformTextures, Quad, ScaledPixels, Scene, ShaderProgram, TextureFormat, TextureId,
        point, rgb, size,
    };

    const COPY: &str = "fn shade(input: ShadeInput) -> vec4<f32> {\n\
        return textureLoad(t0, vec2<i32>(input.local), 0);\n\
    }";
    const TINT: &str = "struct ShaderParams { color: vec4<f32> }\n\
        fn shade(input: ShadeInput) -> vec4<f32> { return params.color; }";

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds::new(
            point(ScaledPixels(x), ScaledPixels(y)),
            size(ScaledPixels(width), ScaledPixels(height)),
        )
    }

    fn device_rect(x: i32, y: i32, width: i32, height: i32) -> Bounds<DevicePixels> {
        Bounds::new(
            point(DevicePixels(x), DevicePixels(y)),
            size(DevicePixels(width), DevicePixels(height)),
        )
    }

    fn shader(
        program: &ShaderProgram,
        area: Bounds<ScaledPixels>,
        params: &[u8],
        textures: &[TextureId],
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

    fn render(
        renderer: &mut WgpuHeadlessRenderer,
        scene: &mut Scene,
        side: i32,
    ) -> image::RgbaImage {
        scene.finish();
        renderer
            .render_scene_to_image(scene, size(DevicePixels(side), DevicePixels(side)))
            .unwrap()
    }

    #[test]
    fn srgb_texels_round_trip_exactly() {
        let mut renderer = WgpuHeadlessRenderer::new().unwrap();
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
            .update(texture, device_rect(0, 0, 16, 16), &bytes, 64)
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
    fn sixteen_bit_texels_are_read_as_unorm() {
        let mut renderer = WgpuHeadlessRenderer::new().unwrap();
        let textures = renderer.textures().clone();
        let Ok(texture) = textures.create(
            size(DevicePixels(2), DevicePixels(1)),
            TextureFormat::Rgba16Unorm,
        ) else {
            return;
        };
        let texels: [[u16; 4]; 2] = [[65535, 0, 32896, 65535], [0, 65535, 0, 65535]];
        let bytes: Vec<u8> = texels
            .iter()
            .flatten()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        textures
            .update(texture, device_rect(0, 0, 2, 1), &bytes, 16)
            .unwrap();

        let program = ShaderProgram::new(COPY);
        let mut scene = Scene::default();
        scene.insert_primitive(shader(
            &program,
            bounds(0., 0., 2., 1.),
            &color_params([0.; 4]),
            &[texture],
        ));
        let image = render(&mut renderer, &mut scene, 2);
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 128, 255]);
        assert_eq!(image.get_pixel(1, 0).0, [0, 255, 0, 255]);
    }

    #[test]
    fn partial_update_reads_strided_source() {
        let mut renderer = WgpuHeadlessRenderer::new().unwrap();
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
                device_rect(0, 0, 8, 8),
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
                device_rect(1, 4, 2, 3),
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
        let mut renderer = WgpuHeadlessRenderer::new().unwrap();
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
        let mut renderer = WgpuHeadlessRenderer::new().unwrap();
        let program = ShaderProgram::new(TINT);
        let mut scene = Scene::default();
        scene.insert_primitive(shader(&program, bounds(0., 0., 4., 4.), &[0; 8], &[]));
        let image = render(&mut renderer, &mut scene, 4);
        assert!(image.pixels().all(|pixel| pixel.0 == [0, 0, 0, 255]));
    }

    #[test]
    fn invalid_program_draws_nothing() {
        let mut renderer = WgpuHeadlessRenderer::new().unwrap();
        let program =
            ShaderProgram::new("fn shade(input: ShadeInput) -> vec4<f32> { return nope; }");
        let mut scene = Scene::default();
        scene.insert_primitive(shader(&program, bounds(0., 0., 4., 4.), &[0; 16], &[]));
        let image = render(&mut renderer, &mut scene, 4);
        assert!(image.pixels().all(|pixel| pixel.0 == [0, 0, 0, 255]));
    }
}
