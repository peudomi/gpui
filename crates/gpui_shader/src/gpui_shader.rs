use anyhow::{Context as _, Result, anyhow};
use naga::{
    ResourceBinding,
    valid::{Capabilities, ModuleInfo, ValidationFlags, Validator},
};

const MASK_SOURCE: &str = include_str!("mask.wgsl");
const QUAD_SOURCE: &str = include_str!("quad.wgsl");
const DEFAULT_PARAMS: &str = "struct ShaderParams { unused: vec4<f32> }\n";

pub const VERTEX_ENTRY: &str = "gpui_shader_vertex";
pub const FRAGMENT_ENTRY: &str = "gpui_shader_fragment";
pub const TEXTURE_COUNT: usize = 4;

#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct ShaderQuadInstance {
    pub viewport_size: [f32; 2],
    pub opacity: f32,
    pub pad: f32,
    pub bounds: [f32; 4],
    pub content_mask_bounds: [f32; 4],
    pub content_mask_corner_radii: [f32; 4],
    pub content_mask_corner_smoothing: f32,
    pub content_mask_pad: f32,
}

pub struct ShaderModule {
    source: String,
    module: naga::Module,
    info: ModuleInfo,
    params_size: u32,
}

impl ShaderModule {
    pub fn new(app_source: &str) -> Result<Self> {
        let mut source = String::with_capacity(
            app_source.len() + MASK_SOURCE.len() + QUAD_SOURCE.len() + DEFAULT_PARAMS.len() + 2,
        );
        source.push_str(app_source);
        source.push('\n');
        if !declares_params(app_source) {
            source.push_str(DEFAULT_PARAMS);
        }
        source.push_str(QUAD_SOURCE);
        source.push('\n');
        source.push_str(MASK_SOURCE);

        let module = naga::front::wgsl::parse_str(&source)
            .map_err(|error| anyhow!("{}", error.emit_to_string(&source)))?;
        let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(&module)
            .map_err(|error| anyhow!("{}", error.emit_to_string(&source)))?;
        let params_size = params_size(&module)?;
        Ok(Self {
            source,
            module,
            info,
            params_size,
        })
    }

    pub fn wgsl(&self) -> &str {
        &self.source
    }

    pub fn params_size(&self) -> u32 {
        self.params_size
    }

    #[cfg(feature = "msl")]
    pub fn to_msl(&self) -> Result<Msl> {
        use crate::msl;

        let mut resources = msl::BindingMap::new();
        resources.insert(
            binding(0),
            msl::BindTarget {
                buffer: Some(msl::INSTANCE_BUFFER),
                ..Default::default()
            },
        );
        resources.insert(
            binding(1),
            msl::BindTarget {
                buffer: Some(msl::PARAMS_BUFFER),
                ..Default::default()
            },
        );
        for index in 0..TEXTURE_COUNT as u8 {
            resources.insert(
                binding(2 + index as u32),
                msl::BindTarget {
                    texture: Some(index),
                    ..Default::default()
                },
            );
        }
        let entry_resources = msl::EntryPointResources {
            resources,
            immediates_buffer: None,
            sizes_buffer: None,
        };
        let options = msl::Options {
            lang_version: (2, 4),
            per_entry_point_map: [VERTEX_ENTRY, FRAGMENT_ENTRY]
                .into_iter()
                .map(|name| (name.to_string(), entry_resources.clone()))
                .collect(),
            fake_missing_bindings: false,
            zero_initialize_workgroup_memory: false,
            ..Default::default()
        };
        let pipeline_options = msl::PipelineOptions {
            vertex_pulling_transform: false,
            ..Default::default()
        };
        let (source, translation) =
            msl::write_string(&self.module, &self.info, &options, &pipeline_options)
                .context("translating shader to MSL")?;
        let mut vertex_entry = None;
        let mut fragment_entry = None;
        for (entry_point, name) in self
            .module
            .entry_points
            .iter()
            .zip(translation.entry_point_names)
        {
            let name = name.with_context(|| format!("translating {}", entry_point.name))?;
            match entry_point.name.as_str() {
                VERTEX_ENTRY => vertex_entry = Some(name),
                FRAGMENT_ENTRY => fragment_entry = Some(name),
                _ => {}
            }
        }
        Ok(Msl {
            source,
            vertex_entry: vertex_entry.context("missing vertex entry point")?,
            fragment_entry: fragment_entry.context("missing fragment entry point")?,
        })
    }
}

#[cfg(feature = "msl")]
pub struct Msl {
    pub source: String,
    pub vertex_entry: String,
    pub fragment_entry: String,
}

#[cfg(feature = "msl")]
pub mod msl {
    pub use naga::back::msl::*;

    pub const INSTANCE_BUFFER: Slot = 0;
    pub const PARAMS_BUFFER: Slot = 1;
}

fn binding(binding: u32) -> ResourceBinding {
    ResourceBinding { group: 0, binding }
}

fn declares_params(source: &str) -> bool {
    source
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|word| word == "ShaderParams")
}

fn params_size(module: &naga::Module) -> Result<u32> {
    let params = module
        .global_variables
        .iter()
        .find(|(_, global)| global.name.as_deref() == Some("params"))
        .context("missing params global")?
        .1;
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|error| anyhow!("laying out shader types: {error}"))?;
    Ok(layouter[params.ty].size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOLID: &str = "fn shade(input: ShadeInput) -> vec4<f32> { return vec4<f32>(1.0); }";

    #[test]
    fn instance_layout_matches_wgsl() {
        let module = ShaderModule::new(SOLID).unwrap();
        let instance = module
            .module
            .global_variables
            .iter()
            .find(|(_, global)| global.name.as_deref() == Some("gpui_instance"))
            .unwrap()
            .1;
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.module.to_ctx()).unwrap();
        assert_eq!(
            layouter[instance.ty].size as usize,
            std::mem::size_of::<ShaderQuadInstance>()
        );
    }

    #[test]
    fn params_size_follows_app_struct() {
        let source = format!(
            "struct ShaderParams {{ scale: f32, origin: vec2<f32>, tint: vec4<f32> }}\n{SOLID}"
        );
        assert_eq!(ShaderModule::new(&source).unwrap().params_size(), 32);
        assert_eq!(ShaderModule::new(SOLID).unwrap().params_size(), 16);
    }

    #[test]
    fn errors_point_at_app_lines() {
        let error =
            ShaderModule::new("fn shade(input: ShadeInput) -> vec4<f32> {\n    return nope;\n}")
                .err()
                .unwrap()
                .to_string();
        assert!(error.contains(":2:"), "{error}");
    }

    #[test]
    fn mask_source_matches_renderer_shaders() {
        let renderer = include_str!("../../gpui_wgpu/src/shaders.wgsl");
        let mut rest = MASK_SOURCE;
        while let Some(start) = rest
            .find("\nfn ")
            .or_else(|| rest.starts_with("fn ").then_some(0))
        {
            let start = if rest[start..].starts_with('\n') {
                start + 1
            } else {
                start
            };
            let end = rest[start..]
                .find("\n}\n")
                .map(|end| start + end + 3)
                .unwrap_or(rest.len());
            let function = &rest[start..end];
            assert!(
                renderer.contains(function),
                "mask.wgsl diverged from gpui_wgpu/src/shaders.wgsl:\n{function}"
            );
            rest = &rest[end..];
        }
    }

    #[cfg(feature = "msl")]
    #[test]
    fn translates_to_msl() {
        let source = "struct ShaderParams { tint: vec4<f32> }\n\
             fn shade(input: ShadeInput) -> vec4<f32> {\n\
                 let texel = textureLoad(t0, vec2<i32>(input.local), 0);\n\
                 return texel * params.tint;\n\
             }";
        let msl = ShaderModule::new(source).unwrap().to_msl().unwrap();
        assert!(msl.source.contains("[[buffer(1)]]"), "{}", msl.source);
        assert!(msl.source.contains("[[texture(0)]]"), "{}", msl.source);
    }
}
