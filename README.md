# gpui (fork)

This repository is a fork of [GPUI](https://www.gpui.rs), the GPU-accelerated UI
framework for Rust, extracted from the [Zed](https://github.com/zed-industries/zed)
monorepo by Zed Industries, Inc.

## Notice of modifications

Per the Apache License 2.0 §4(b), this fork has been modified from the upstream
source (`zed-industries/zed`):

- Everything unrelated to GPUI has been removed. Only the `gpui*` crates, their
  in-repo dependencies (`collections`, `refineable`, `http_client`,
  `http_client_tls`, `media`, `path`, `reqwest_client`, `scheduler`, `sum_tree`,
  `util`, `util_macros`, `tooling/perf`), and the build configuration remain.
- The upstream `ztracing`, `ztracing_macro`, and `zlog` crates (GPL-3.0-or-later)
  have been removed entirely, along with their few call sites: the
  `#[ztracing::instrument]` profiling attributes in `gpui` and `sum_tree` and a
  test-only logger hook in `sum_tree`.
- The root workspace manifest, `.cargo` config, and repository scaffolding were
  trimmed accordingly.
- Wide-gamut image rendering was added: `RenderImageWide` (premultiplied RGBA
  f16, extended sRGB) with `Window::paint_image_wide` / `drop_image_wide`, an
  `AtlasTextureKind::PolychromeWide` half-float atlas on every backend, an
  `RGBA16Float` framebuffer tagged `kCGColorSpaceExtendedSRGB` on macOS/Metal,
  and an `Rgba16Float` surface preference (with 8-bit fallback) on wgpu. On
  Windows/DirectX, the scene renders into an offscreen `R16G16B16A16_FLOAT`
  target (sRGB-encoded, so blending is unchanged) and a final `wide_present`
  pass converts per the window's monitor, mirroring Chromium: with advanced
  color (HDR/ACM) enabled it applies the sign-preserving extended sRGB EOTF
  into an f16 swap chain tagged `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`
  (scRGB) for the OS to color-manage; on SDR monitors it instead maps colors
  through a 3x3 matrix derived from the monitor ICC profile's primaries
  (transfer curve assumed sRGB, as Chromium does) into an 8-bit swap chain.
  The mode re-resolves when the window moves between monitors and on
  `WM_DISPLAYCHANGE`/`WM_SETTINGCHANGE` (HDR/ACM toggles, ICC profile
  reassignment); `GPUI_COLOR_MODE=scrgb|srgb` overrides detection for
  debugging.

- App-private platform drag sessions were added for Chrome-style tab tear-off.
  `ExternalDragPayload::AppPrivate` starts an OS drag session carrying only a
  per-app private pasteboard type (`app.gpui.private-drag.<bundle id>`) that
  other applications do not accept; gpui windows register that type and synthesize an empty-path
  `FileDropEvent::Entered` for it, and a platform-owned drag now restores its
  in-app payload in any window of the app
  (`PlatformOwnedDragState::Restored { window }`), not only the source window.
  `Window::promote_active_drag_to_platform` hands the active drag to the
  platform immediately instead of waiting for the pointer to leave the
  viewport, and `FileDropEvent::SessionMoved` with
  `App::set_platform_drag_moved_handler` reports the session's global pointer
  position (from `draggingSession:movedToPoint:` on macOS). Supporting window
  APIs: `PlatformWindow::move_to` (programmatic placement),
  `PlatformWindow::set_accepts_drags` (a window dragged along with the pointer
  opts out of drop destination routing), and `WindowKind::Overlay`, a
  chrome-less, shadowless, non-activating always-on-top surface excluded from
  drag destination routing and mouse hit-testing. All of this is implemented
  on macOS; Windows/X11 map `Overlay` to their popup-style windows, the
  Wayland backend declines `AppPrivate` sources, and the web backend rejects
  `Overlay`.

These changes are documented in detail, with code, rationale, and open
questions, in [docs/fork](docs/fork/README.md).

Further modifications will be tracked in this repository's git history.

## License

Apache License 2.0. See [LICENSE-APACHE](LICENSE-APACHE).

Copyright of the original code remains with Zed Industries, Inc. and the Zed
contributors. This fork contains no GPL-licensed code: every remaining crate is
licensed under Apache-2.0, and third-party dependencies are permissively
licensed (MIT/Apache/BSD/MPL-2.0 and similar).

"Zed" and "GPUI" are trademarks of Zed Industries, Inc.; this fork is not
affiliated with or endorsed by Zed Industries.

## Upstream

The `upstream` remote tracks `zed-industries/zed`. Only GPUI-related changes are
merged from upstream; the merge workflow, conflict-resolution rules, and the
mandatory post-merge license audit (`script/check-licenses`) are documented in
[UPSTREAM.md](UPSTREAM.md).

```shell
cargo check -p gpui        # build the library
cargo run -p gpui --example hello_world
```
