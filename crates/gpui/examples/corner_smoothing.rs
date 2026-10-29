#![cfg_attr(target_family = "wasm", no_main)]

#[path = "example_support/fonts.rs"]
mod example_support;

use std::{path::Path, sync::Arc};

use gpui::{
    App, Bounds, BoxShadow, Context, Div, SharedString, Window, WindowBounds, WindowOptions, div,
    hsla, img, prelude::*, px, rgb, size,
};
use gpui_platform::application;

const SMOOTHING: [f32; 3] = [0., 0.6, 1.];

struct CornerSmoothing {}

fn card(smoothing: f32) -> Div {
    div()
        .size(px(96.))
        .bg(rgb(0x4f7cff))
        .rounded(px(28.))
        .rounded_smoothing(smoothing)
}

fn example(label: impl Into<SharedString>, example: impl IntoElement) -> impl IntoElement {
    let label = label.into();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .w(px(150.))
        .child(example)
        .child(div().text_xs().text_color(rgb(0x555555)).child(label))
}

fn row(title: &'static str, make: impl Fn(f32) -> Div) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_4()
        .py_3()
        .border_b_1()
        .border_color(hsla(0., 0., 0., 0.1))
        .child(div().w(px(110.)).text_sm().child(title))
        .children(
            SMOOTHING
                .into_iter()
                .map(|smoothing| example(format!("{smoothing}"), make(smoothing))),
        )
}

impl Render for CornerSmoothing {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let image: Arc<Path> = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/image/app-icon.png"
        ))
        .into();

        let left = div()
            .flex()
            .flex_col()
            .child(row("Fill", card))
            .child(row("Border", |smoothing| {
                card(smoothing)
                    .bg(rgb(0xffffff))
                    .border_4()
                    .border_color(rgb(0x1f2937))
            }))
            .child(row("Dashed", |smoothing| {
                card(smoothing)
                    .bg(rgb(0xffffff))
                    .border_2()
                    .border_dashed()
                    .border_color(rgb(0x1f2937))
            }))
            .child(row("Uneven", |smoothing| {
                div()
                    .size(px(96.))
                    .bg(rgb(0xffe08a))
                    .rounded_tl(px(40.))
                    .rounded_tr(px(8.))
                    .rounded_br(px(24.))
                    .border_l_4()
                    .border_t_1()
                    .border_r_2()
                    .border_b_1()
                    .border_color(rgb(0x7c4a03))
                    .rounded_smoothing(smoothing)
            }))
            .child(row("Capsule", |smoothing| {
                div()
                    .w(px(130.))
                    .h(px(48.))
                    .bg(rgb(0x34c759))
                    .rounded_full()
                    .rounded_smoothing(smoothing)
            }));
        let right = div()
            .flex()
            .flex_col()
            .child(row("Drop shadow", |smoothing| {
                card(smoothing).bg(rgb(0xffffff)).shadow(vec![
                    BoxShadow::new(px(0.), px(8.), hsla(0., 0., 0., 0.35)).blur_radius(px(12.)),
                ])
            }))
            .child(row("Hard shadow", |smoothing| {
                card(smoothing)
                    .bg(rgb(0xffffff))
                    .shadow(vec![BoxShadow::new(px(6.), px(6.), hsla(0., 0., 0., 0.8))])
            }))
            .child(row("Inset shadow", |smoothing| {
                card(smoothing).bg(rgb(0xffffff)).shadow(vec![
                    BoxShadow::new(px(0.), px(4.), hsla(0., 0., 0., 0.5))
                        .blur_radius(px(8.))
                        .inset(),
                ])
            }))
            .child(row("Overflow mask", |smoothing| {
                card(smoothing)
                    .overflow_hidden()
                    .child(div().size_full().bg(rgb(0xff9500)))
            }))
            .child(row("Image", move |smoothing| {
                div().child(
                    img(image.clone())
                        .size(px(96.))
                        .rounded(px(28.))
                        .rounded_smoothing(smoothing),
                )
            }));

        div()
            .id("root")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(0xf4f4f6))
            .p_6()
            .flex()
            .gap_8()
            .child(left)
            .child(right)
    }
}

fn run_example() {
    application().run(|cx: &mut App| {
        if !example_support::load_fonts(cx) {
            return;
        }
        let bounds = Bounds::centered(None, size(px(1300.0), px(860.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| CornerSmoothing {}),
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
