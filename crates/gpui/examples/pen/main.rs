//! Pen input test bench for GPUI.
//!
//! The canvas on the left claims pen strokes and draws them. The panel on the right never
//! handles pen events, so pen input over it falls back to touch and mouse input.

mod app;

use gpui::{App, AppContext as _, WindowOptions};

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| app::PenExample::new(window, cx))
        })
        .unwrap();
        cx.activate(true);
    });
}
