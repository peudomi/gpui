struct ShaderQuadInstance {
    viewport_size: vec2<f32>,
    opacity: f32,
    pad: f32,
    bounds: Bounds,
    content_mask: ContentMask,
}

struct ShadeInput {
    pixel: vec2<f32>,
    local: vec2<f32>,
    size: vec2<f32>,
}

@group(0) @binding(0) var<uniform> gpui_instance: ShaderQuadInstance;
@group(0) @binding(1) var<uniform> params: ShaderParams;
@group(0) @binding(2) var t0: texture_2d<f32>;
@group(0) @binding(3) var t1: texture_2d<f32>;
@group(0) @binding(4) var t2: texture_2d<f32>;
@group(0) @binding(5) var t3: texture_2d<f32>;

struct GpuiShaderVarying {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn gpui_shader_vertex(@builtin(vertex_index) vertex_id: u32) -> GpuiShaderVarying {
    let bounds = gpui_instance.bounds;
    let clip = gpui_instance.content_mask.bounds;
    let low = max(bounds.origin, clip.origin);
    let high = max(low, min(bounds.origin + bounds.size, clip.origin + clip.size));
    let unit = vec2<f32>(f32(vertex_id & 1u), f32((vertex_id >> 1u) & 1u));
    let position = mix(low, high, unit);
    let device = position / gpui_instance.viewport_size * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    var out: GpuiShaderVarying;
    out.position = vec4<f32>(device, 0.0, 1.0);
    return out;
}

@fragment
fn gpui_shader_fragment(input: GpuiShaderVarying) -> @location(0) vec4<f32> {
    let pixel = input.position.xy;
    var shade_input: ShadeInput;
    shade_input.pixel = pixel;
    shade_input.local = pixel - gpui_instance.bounds.origin;
    shade_input.size = gpui_instance.bounds.size;
    let color = shade(shade_input);
    let alpha = color.a * gpui_instance.opacity
        * content_mask_alpha(pixel, gpui_instance.content_mask);
    return vec4<f32>(color.rgb, alpha);
}
