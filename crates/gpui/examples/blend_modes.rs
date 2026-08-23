#![cfg_attr(target_family = "wasm", no_main)]

//! Demonstrates the fixed-function quad blend modes: a row of colored quads is
//! painted over a background gradient with each [`BlendMode`], including the
//! `Invert` mode used for e.g. brush cursors that must stay visible over any
//! background.

use gpui::{
    App, BlendMode, Bounds, Context, Render, Window, WindowBounds, WindowOptions, canvas, div,
    fill, linear_color_stop, linear_gradient, prelude::*, px, rgb, size,
};
use gpui_platform::application;

struct BlendModes;

impl Render for BlendModes {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            canvas(
                |_, _, _| {},
                |bounds, _, window, _| {
                    window.paint_quad(fill(
                        bounds,
                        linear_gradient(
                            90.,
                            linear_color_stop(rgb(0x000000), 0.),
                            linear_color_stop(rgb(0xffffff), 1.),
                        ),
                    ));

                    let modes = [
                        BlendMode::Normal,
                        BlendMode::Additive,
                        BlendMode::Multiply,
                        BlendMode::Screen,
                        BlendMode::Invert,
                    ];
                    let quad_size = px(64.);
                    let gap = px(16.);
                    for (i, mode) in modes.into_iter().enumerate() {
                        let origin = bounds.origin
                            + gpui::point(gap + (quad_size + gap) * i as f32, px(100.));
                        let quad_bounds = Bounds::new(origin, size(quad_size, quad_size));
                        window.paint_quad(fill(quad_bounds, rgb(0xff8040)).blend_mode(mode));

                        let white_origin = origin + gpui::point(px(0.), quad_size + gap);
                        let white_bounds = Bounds::new(white_origin, size(quad_size, quad_size));
                        window.paint_quad(fill(white_bounds, gpui::white()).blend_mode(mode));
                    }
                },
            )
            .size_full(),
        )
    }
}

fn run_example() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(500.), px(300.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| BlendModes),
        )
        .unwrap();
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
