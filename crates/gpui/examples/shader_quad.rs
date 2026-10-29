#![cfg_attr(target_family = "wasm", no_main)]

use std::{cell::RefCell, rc::Rc};

use gpui::{
    App, Bounds, Context, DevicePixels, ShaderProgram, TextureFormat, TextureId, Window,
    WindowBounds, WindowOptions, canvas, div, point, prelude::*, px, rgb, size,
};
use gpui_platform::application;

const DOCUMENT_SIZE: i32 = 64;
const BRUSH_SIZE: i32 = 6;

const SHADER: &str = r#"
struct ShaderParams {
    scale: f32,
    pad: f32,
    offset: vec2<f32>,
}

fn shade(input: ShadeInput) -> vec4<f32> {
    let checker = select(0.8, 1.0, ((u32(input.pixel.x) / 16u + u32(input.pixel.y) / 16u) & 1u) == 0u);
    let document = floor((input.local - params.offset) / params.scale);
    let dimensions = vec2<f32>(textureDimensions(t0));
    if (any(document < vec2<f32>(0.0)) || any(document >= dimensions)) {
        return vec4<f32>(vec3<f32>(checker), 1.0);
    }
    let texel = textureLoad(t0, vec2<i32>(document), 0);
    var color = texel.rgb + vec3<f32>(checker) * (1.0 - texel.a);
    let left = floor((input.local - vec2<f32>(1.0, 0.0) - params.offset) / params.scale);
    let up = floor((input.local - vec2<f32>(0.0, 1.0) - params.offset) / params.scale);
    if (params.scale >= 5.0 && (left.x != document.x || up.y != document.y)) {
        color = color * (217.0 / 255.0);
    }
    return vec4<f32>(color, 1.0);
}
"#;

struct Document {
    texture: Option<TextureId>,
    pixels: Vec<u8>,
    frame: u32,
}

impl Document {
    fn new() -> Self {
        let mut pixels = vec![0; (DOCUMENT_SIZE * DOCUMENT_SIZE * 4) as usize];
        for y in 0..DOCUMENT_SIZE {
            for x in 0..DOCUMENT_SIZE {
                let index = ((y * DOCUMENT_SIZE + x) * 4) as usize;
                let alpha = if x < DOCUMENT_SIZE / 2 { 255 } else { 128 };
                let red = (x * 255 / DOCUMENT_SIZE) as u32 * alpha / 255;
                let green = (y * 255 / DOCUMENT_SIZE) as u32 * alpha / 255;
                pixels[index..index + 4].copy_from_slice(&[0, green as u8, red as u8, alpha as u8]);
            }
        }
        Self {
            texture: None,
            pixels,
            frame: 0,
        }
    }

    fn stamp(&mut self, window: &mut Window) -> anyhow::Result<()> {
        let texture = match self.texture {
            Some(texture) => texture,
            None => {
                let texture = window.create_texture(
                    size(DevicePixels(DOCUMENT_SIZE), DevicePixels(DOCUMENT_SIZE)),
                    TextureFormat::Bgra8Unorm,
                )?;
                window.update_texture(
                    texture,
                    Bounds::new(
                        point(DevicePixels(0), DevicePixels(0)),
                        size(DevicePixels(DOCUMENT_SIZE), DevicePixels(DOCUMENT_SIZE)),
                    ),
                    &self.pixels,
                    (DOCUMENT_SIZE * 4) as usize,
                )?;
                self.texture = Some(texture);
                texture
            }
        };

        self.frame += 1;
        let span = DOCUMENT_SIZE - BRUSH_SIZE;
        let step = (self.frame / 2) as i32;
        let x = step % span;
        let y = (step / span * 7) % span;
        for row in y..y + BRUSH_SIZE {
            for column in x..x + BRUSH_SIZE {
                let index = ((row * DOCUMENT_SIZE + column) * 4) as usize;
                self.pixels[index..index + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let offset = ((y * DOCUMENT_SIZE + x) * 4) as usize;
        window.update_texture(
            texture,
            Bounds::new(
                point(DevicePixels(x), DevicePixels(y)),
                size(DevicePixels(BRUSH_SIZE), DevicePixels(BRUSH_SIZE)),
            ),
            &self.pixels[offset..],
            (DOCUMENT_SIZE * 4) as usize,
        )
    }
}

struct ShaderQuadExample {
    program: ShaderProgram,
    document: Rc<RefCell<Document>>,
}

fn params(scale: f32, offset: [f32; 2]) -> Vec<u8> {
    [scale, 0., offset[0], offset[1]]
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect()
}

impl ShaderQuadExample {
    fn shader(&self, scale: f32) -> impl IntoElement {
        let program = self.program.clone();
        let document = self.document.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let mut document = document.borrow_mut();
                if let Err(error) = document.stamp(window) {
                    eprintln!("updating texture: {error:#}");
                    return;
                }
                let scale = scale * window.scale_factor();
                let textures = [document.texture.unwrap()];
                if let Err(error) =
                    window.paint_shader(bounds, &program, &params(scale, [12., 12.]), &textures)
                {
                    eprintln!("painting shader: {error:#}");
                }
            },
        )
        .size_full()
    }
}

impl Render for ShaderQuadExample {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        div()
            .size_full()
            .bg(rgb(0xe4e4e8))
            .p_6()
            .flex()
            .gap_6()
            .child(
                div()
                    .relative()
                    .size(px(420.))
                    .rounded(px(36.))
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_4()
                            .left_4()
                            .size(px(120.))
                            .bg(rgb(0x2563eb)),
                    )
                    .child(div().absolute().size_full().child(self.shader(6.)))
                    .child(
                        div()
                            .absolute()
                            .bottom_6()
                            .right_6()
                            .size(px(80.))
                            .rounded(px(12.))
                            .bg(rgb(0xef4444)),
                    ),
            )
            .child(
                div().relative().size(px(260.)).bg(rgb(0x16a34a)).child(
                    div()
                        .absolute()
                        .size_full()
                        .opacity(0.5)
                        .rounded(px(60.))
                        .rounded_smoothing(1.)
                        .overflow_hidden()
                        .child(self.shader(2.)),
                ),
            )
    }
}

fn run_example() {
    application().run(|cx: &mut App| {
        let program = ShaderProgram::new(SHADER);
        for index in 0..2 {
            let origin = point(px(80. + index as f32 * 120.), px(80. + index as f32 * 120.));
            let bounds = Bounds::new(origin, size(px(780.), px(500.)));
            let program = program.clone();
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                move |_, cx| {
                    cx.new(|_| ShaderQuadExample {
                        program,
                        document: Rc::new(RefCell::new(Document::new())),
                    })
                },
            )
            .unwrap();
        }
        cx.activate(true);
    });
}

#[cfg(not(target_family = "wasm"))]
fn main() {
    run_example();
}

#[cfg(target_family = "wasm")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    gpui_platform::web_init();
    run_example();
}
