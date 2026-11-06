use std::{ffi::CString, mem, ptr, slice, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use collections::FxHashMap;
use gpui::{
    Bounds, DevicePixels, MAX_SHADER_PARAMS, MAX_SHADER_TEXTURES, PaintShader, PlatformTextures,
    Size, TextureFormat, TextureId,
};
use gpui_shader::{ShaderModule, ShaderQuadInstance, hlsl};
use parking_lot::Mutex;
use windows::{
    Win32::Graphics::{
        Direct3D::{
            D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
            Fxc::{D3DCOMPILE_IEEE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile},
            ID3DBlob,
        },
        Direct3D11::*,
        Dxgi::Common::{
            DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_UNORM,
            DXGI_SAMPLE_DESC,
        },
    },
    core::PCSTR,
};

const MAX_TEXTURE_DIMENSION: i32 = D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION as i32;
const INSTANCE_BUFFER_SIZE: usize = mem::size_of::<ShaderQuadInstance>().next_multiple_of(16);

pub(crate) struct DirectXTextures(Mutex<TexturesState>);

struct TexturesState {
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    next_id: u64,
    generation: u64,
    textures: FxHashMap<TextureId, Texture>,
}

struct Texture {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    size: Size<DevicePixels>,
    format: TextureFormat,
}

impl DirectXTextures {
    fn new(device: &ID3D11Device, device_context: &ID3D11DeviceContext) -> Self {
        Self(Mutex::new(TexturesState {
            device: device.clone(),
            device_context: device_context.clone(),
            next_id: 0,
            generation: 0,
            textures: FxHashMap::default(),
        }))
    }

    fn handle_device_lost(&self, device: &ID3D11Device, device_context: &ID3D11DeviceContext) {
        let mut state = self.0.lock();
        state.device = device.clone();
        state.device_context = device_context.clone();
        state.textures.clear();
        state.generation += 1;
    }

    fn view(&self, id: TextureId) -> Option<ID3D11ShaderResourceView> {
        self.0
            .lock()
            .textures
            .get(&id)
            .map(|texture| texture.view.clone())
    }
}

impl PlatformTextures for DirectXTextures {
    fn create(&self, size: Size<DevicePixels>, format: TextureFormat) -> Result<TextureId> {
        anyhow::ensure!(
            size.width.0 > 0
                && size.height.0 > 0
                && size.width.0 <= MAX_TEXTURE_DIMENSION
                && size.height.0 <= MAX_TEXTURE_DIMENSION,
            "texture size {size:?} is outside 1..={MAX_TEXTURE_DIMENSION}"
        );
        let mut state = self.0.lock();
        let (texture, view) = create_texture(
            &state.device,
            size.width.0 as u32,
            size.height.0 as u32,
            dxgi_format(format),
            None,
        )
        .context("allocating shader texture")?;
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
        let destination = D3D11_BOX {
            left: rect.origin.x.0 as u32,
            top: rect.origin.y.0 as u32,
            front: 0,
            right: (rect.origin.x.0 + rect.size.width.0) as u32,
            bottom: (rect.origin.y.0 + rect.size.height.0) as u32,
            back: 1,
        };
        // SAFETY: The box lies within the texture and `bytes` holds `height` rows of
        // `bytes_per_row`, the last one at least `row_len` long, as checked above.
        unsafe {
            state.device_context.UpdateSubresource(
                &texture.texture,
                0,
                Some(&destination),
                bytes.as_ptr().cast(),
                bytes_per_row as u32,
                0,
            );
        }
        Ok(())
    }

    fn remove(&self, id: TextureId) {
        self.0.lock().textures.remove(&id);
    }

    fn generation(&self) -> u64 {
        self.0.lock().generation
    }
}

fn dxgi_format(format: TextureFormat) -> DXGI_FORMAT {
    match format {
        TextureFormat::Bgra8Unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
        TextureFormat::Rgba16Unorm => DXGI_FORMAT_R16G16B16A16_UNORM,
    }
}

fn create_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    initial: Option<&D3D11_SUBRESOURCE_DATA>,
) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    let mut view = None;
    // SAFETY: When given, `initial` covers every row of the texture.
    unsafe {
        device.CreateTexture2D(&desc, initial.map(ptr::from_ref), Some(&mut texture))?;
        let texture = texture.as_ref().context("texture missing")?;
        device.CreateShaderResourceView(texture, None, Some(&mut view))?;
    }
    Ok((
        texture.context("texture missing")?,
        view.context("texture view missing")?,
    ))
}

struct Pipeline {
    vertex: ID3D11VertexShader,
    fragment: ID3D11PixelShader,
    params_size: usize,
}

struct DeviceObjects {
    instance_buffer: ID3D11Buffer,
    params_buffer: ID3D11Buffer,
    blend_state: ID3D11BlendState,
    empty_view: ID3D11ShaderResourceView,
}

pub(crate) struct ShaderQuads {
    textures: Arc<DirectXTextures>,
    supported: bool,
    objects: DeviceObjects,
    pipelines: FxHashMap<u64, Option<Pipeline>>,
    reported_params: FxHashMap<u64, usize>,
}

impl ShaderQuads {
    pub(crate) fn new(device: &ID3D11Device, device_context: &ID3D11DeviceContext) -> Result<Self> {
        Ok(Self {
            textures: Arc::new(DirectXTextures::new(device, device_context)),
            supported: is_supported(device),
            objects: DeviceObjects::new(device)?,
            pipelines: FxHashMap::default(),
            reported_params: FxHashMap::default(),
        })
    }

    pub(crate) fn textures(&self) -> Option<Arc<dyn PlatformTextures>> {
        self.supported
            .then(|| self.textures.clone() as Arc<dyn PlatformTextures>)
    }

    pub(crate) fn handle_device_lost(
        &mut self,
        device: &ID3D11Device,
        device_context: &ID3D11DeviceContext,
    ) -> Result<()> {
        self.textures.handle_device_lost(device, device_context);
        self.supported = is_supported(device);
        self.objects = DeviceObjects::new(device)?;
        self.pipelines.clear();
        Ok(())
    }

    pub(crate) fn draw(
        &mut self,
        shaders: &[PaintShader],
        device: &ID3D11Device,
        device_context: &ID3D11DeviceContext,
        viewport_size: [f32; 2],
    ) -> Result<()> {
        if !self.supported || shaders.is_empty() {
            return Ok(());
        }
        let objects = &self.objects;
        // SAFETY: Every bound object is alive for the call and the slots match the
        // registers in `gpui_shader::hlsl`.
        unsafe {
            device_context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            device_context.OMSetBlendState(&objects.blend_state, None, 0xFFFFFFFF);
            device_context.VSSetConstantBuffers(
                hlsl::INSTANCE_REGISTER,
                Some(slice::from_ref(&Some(objects.instance_buffer.clone()))),
            );
            device_context.PSSetConstantBuffers(
                hlsl::INSTANCE_REGISTER,
                Some(&[
                    Some(objects.instance_buffer.clone()),
                    Some(objects.params_buffer.clone()),
                ]),
            );
        }
        for shader in shaders {
            let program = &shader.program;
            let pipeline = self.pipelines.entry(program.id()).or_insert_with(|| {
                build_pipeline(device, program.source())
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

            let instance = instance(shader, viewport_size);
            write_buffer(
                device_context,
                &objects.instance_buffer,
                // SAFETY: `ShaderQuadInstance` is `repr(C)` and made only of `f32`s.
                unsafe {
                    slice::from_raw_parts(
                        ptr::from_ref(&instance).cast::<u8>(),
                        mem::size_of_val(&instance),
                    )
                },
            )?;
            write_buffer(device_context, &objects.params_buffer, &shader.params)?;
            let views: [Option<ID3D11ShaderResourceView>; MAX_SHADER_TEXTURES] =
                std::array::from_fn(|index| {
                    Some(
                        shader.textures[index]
                            .and_then(|id| self.textures.view(id))
                            .unwrap_or_else(|| objects.empty_view.clone()),
                    )
                });
            // SAFETY: The shaders, buffers and views stay alive until the draw is recorded.
            unsafe {
                device_context.VSSetShader(&pipeline.vertex, None);
                device_context.PSSetShader(&pipeline.fragment, None);
                device_context.PSSetShaderResources(hlsl::FIRST_TEXTURE_REGISTER, Some(&views));
                device_context.Draw(4, 0);
            }
        }
        // SAFETY: Unbinding releases the context's references to the application textures.
        unsafe {
            let none: [Option<ID3D11ShaderResourceView>; MAX_SHADER_TEXTURES] = Default::default();
            device_context.PSSetShaderResources(hlsl::FIRST_TEXTURE_REGISTER, Some(&none));
        }
        Ok(())
    }
}

fn is_supported(device: &ID3D11Device) -> bool {
    let supported = unsafe { device.GetFeatureLevel() }.0 >= D3D_FEATURE_LEVEL_11_0.0;
    if !supported {
        log::warn!("shader quads need Direct3D feature level 11_0");
    }
    supported
}

fn instance(shader: &PaintShader, viewport_size: [f32; 2]) -> ShaderQuadInstance {
    let mask = &shader.content_mask;
    ShaderQuadInstance {
        viewport_size,
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
    }
}

impl DeviceObjects {
    fn new(device: &ID3D11Device) -> Result<Self> {
        let blend_state = {
            let mut desc = D3D11_BLEND_DESC::default();
            desc.RenderTarget[0].BlendEnable = true.into();
            desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
            desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
            desc.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
            desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
            desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
            desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
            desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
            let mut state = None;
            unsafe { device.CreateBlendState(&desc, Some(&mut state))? };
            state.context("blend state missing")?
        };
        let zero = [0u8; 4];
        let (_, empty_view) = create_texture(
            device,
            1,
            1,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            Some(&D3D11_SUBRESOURCE_DATA {
                pSysMem: zero.as_ptr().cast(),
                SysMemPitch: 4,
                SysMemSlicePitch: 0,
            }),
        )
        .context("allocating empty shader texture")?;
        Ok(Self {
            instance_buffer: create_constant_buffer(device, INSTANCE_BUFFER_SIZE)?,
            params_buffer: create_constant_buffer(device, MAX_SHADER_PARAMS)?,
            blend_state,
            empty_view,
        })
    }
}

fn create_constant_buffer(device: &ID3D11Device, size: usize) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: size as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let mut buffer = None;
    unsafe { device.CreateBuffer(&desc, None, Some(&mut buffer))? };
    buffer.context("constant buffer missing")
}

fn write_buffer(
    device_context: &ID3D11DeviceContext,
    buffer: &ID3D11Buffer,
    bytes: &[u8],
) -> Result<()> {
    // SAFETY: Both constant buffers are at least as large as any slice written to them:
    // instance records are fixed-size and params are capped at `MAX_SHADER_PARAMS`.
    unsafe {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        device_context.Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
        ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.pData.cast(), bytes.len());
        device_context.Unmap(buffer, 0);
    }
    Ok(())
}

fn build_pipeline(device: &ID3D11Device, source: &str) -> Result<Pipeline> {
    let module = ShaderModule::new(source)?;
    let params_size = module.params_size() as usize;
    anyhow::ensure!(
        params_size <= MAX_SHADER_PARAMS,
        "ShaderParams is {params_size} bytes, above the maximum of {MAX_SHADER_PARAMS}"
    );
    let hlsl = module.to_hlsl()?;
    let vertex_blob = compile(&hlsl.source, &hlsl.vertex_entry, "vs_5_0")?;
    let fragment_blob = compile(&hlsl.source, &hlsl.fragment_entry, "ps_5_0")?;
    let mut vertex = None;
    let mut fragment = None;
    // SAFETY: The blobs hold bytecode that `D3DCompile` just produced for these stages.
    unsafe {
        device.CreateVertexShader(blob_bytes(&vertex_blob), None, Some(&mut vertex))?;
        device.CreatePixelShader(blob_bytes(&fragment_blob), None, Some(&mut fragment))?;
    }
    Ok(Pipeline {
        vertex: vertex.context("vertex shader missing")?,
        fragment: fragment.context("pixel shader missing")?,
        params_size,
    })
}

fn compile(source: &str, entry: &str, target: &str) -> Result<ID3DBlob> {
    let entry = CString::new(entry)?;
    let target = CString::new(target)?;
    let mut code = None;
    let mut errors = None;
    // SAFETY: `source` outlives the call, and the C strings are NUL-terminated.
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR::from_raw(entry.as_ptr().cast()),
            PCSTR::from_raw(target.as_ptr().cast()),
            D3DCOMPILE_IEEE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let message = errors
            .as_ref()
            .map(|blob| String::from_utf8_lossy(unsafe { blob_bytes(blob) }).into_owned())
            .unwrap_or_default();
        return Err(anyhow!("compiling HLSL {target:?}: {error} {message}"));
    }
    code.context("shader bytecode missing")
}

unsafe fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()) }
}
