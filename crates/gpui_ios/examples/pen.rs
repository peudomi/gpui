//! Apple Pencil runner for the pen test bench in `crates/gpui/examples/pen`.

#[cfg(target_os = "ios")]
#[path = "../../gpui/examples/pen/app.rs"]
mod app;

#[cfg(target_os = "ios")]
fn main() {
    use gpui::{App, AppContext as _, WindowOptions};

    gpui_platform::application().run(|cx: &mut App| {
        gpui_ios::ios::set_status_bar_style(gpui_ios::StatusBarContentStyle::Light);
        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| app::PenExample::new(window, cx))
        })
        .unwrap();
        cx.activate(true);
    });
}

#[cfg(not(target_os = "ios"))]
fn main() {
    eprintln!("Build this example for an iOS target.");
}
