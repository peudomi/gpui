# gpui (fork)

This repository is a fork of [GPUI](https://www.gpui.rs), the GPU-accelerated UI
framework for Rust, extracted from the [Zed](https://github.com/zed-industries/zed)
monorepo by Zed Industries, Inc.

## Notice of modifications

Per the Apache License 2.0 §4(b), this fork has been modified from the upstream
source (`zed-industries/zed`):

- Everything unrelated to GPUI has been removed. Only the `gpui*` crates, their
  in-repo dependencies (`bench_metrics`, `collections`, `refineable`,
  `http_client`, `http_client_tls`, `path`, `reqwest_client`, `scheduler`,
  `sum_tree`, `util`, `util_macros`, `tooling/perf`), and the build
  configuration remain.
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
  pass converts per the window's monitor: with advanced
  color (HDR/ACM) enabled it applies the sign-preserving extended sRGB EOTF
  into an f16 swap chain tagged `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`
  (scRGB) for the OS to color-manage; on SDR monitors it instead maps colors
  through a 3x3 matrix derived from the monitor ICC profile's primaries
  (transfer curve assumed sRGB) into an 8-bit swap chain.
  The mode re-resolves when the window moves between monitors and on
  `WM_DISPLAYCHANGE`/`WM_SETTINGCHANGE` (HDR/ACM toggles, ICC profile
  reassignment); `GPUI_COLOR_MODE=scrgb|srgb` overrides detection for
  debugging. The wide-gamut output path is controlled by a new
  `WindowOptions::use_extended_srgb` flag (default `true`). When it is
  `false`, Metal uses a `BGRA8Unorm` layer and honors upstream's
  `use_display_p3`, wgpu drops `Rgba16Float` from its surface preferences, and
  DirectX presents through an 8-bit swap chain with an identity color matrix.
  When both flags are set, extended sRGB takes precedence over Display P3.

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
  position (from `draggingSession:movedToPoint:` on macOS, and
  `IDropSource::QueryContinueDrag` on Windows). Supporting window
  APIs: `PlatformWindow::move_to` (programmatic placement),
  `PlatformWindow::set_accepts_drags` (a window dragged along with the pointer
  opts out of drop destination routing), and `WindowKind::Overlay`, a
  chrome-less, shadowless, unanimated, non-activating always-on-top surface
  excluded from drag destination routing and mouse hit-testing. Implemented on
  macOS and Windows; the Windows backend also gained outbound drag sessions it
  never had (`DoDragDrop` with a gpui-implemented `IDataObject` carrying either
  `CF_HDROP` or the private format). Because `DoDragDrop` runs a modal loop that
  owns the thread, the Windows backend drains the foreground task queue from
  `QueryContinueDrag`, so work spawned by drag handlers runs during the drag
  rather than replaying after the drop, and it disables DWM's open/close
  transition on overlay windows so they appear without animation latency. It
  also honors off-display opening bounds for overlays and for windows opened
  unfocused, so an app can open a window out of sight ahead of the drag that
  needs it: the display fallback that would otherwise recenter them exists for
  restored bounds and a restore always takes focus, and such a window is placed
  with `SetWindowPos` before being shown because `SetWindowPlacement` pulls a
  restored window back onto a monitor, and the placement stashed for a window
  opened with `show: false` is discarded once the app has shown the window
  itself, so a later activation cannot yank it back to its creation bounds. `PlatformWindow::start_window_move`,
  which upstream leaves unimplemented on Windows, now runs the system move loop
  there (`WM_SYSCOMMAND` / `SC_MOVE`), and a new
  `PlatformWindow::on_move_loop_ended` reports when that loop exits, which the
  platform otherwise keeps to itself; Windows raises it from `WM_EXITSIZEMOVE`,
  macOS from a local `NSEventTypeLeftMouseUp` monitor installed when
  `start_window_move` hands the drag to `performWindowDragWithEvent:` (which
  returns immediately and never reports the drag's end), and the remaining
  backends inherit the no-op default. The macOS `start_window_move` also defers
  the AppKit call by one runloop turn and synthesizes the drag event against
  the receiving window from the live cursor, because a window created in the
  current dispatch does not have its final frame yet and AppKit anchors the
  drag against the frame it sees, sending the window to a wrong offset. X11 implements `move_to`,
  `set_accepts_drags`, and `Overlay` (input excluded via an empty SHAPE input
  region) but not the outbound drag session, so `AppPrivate` and `SessionMoved`
  are unavailable there; the Wayland backend declines `AppPrivate` sources, and
  the web backend rejects `Overlay`.

- Content masks can be rounded. `ContentMask` carries `corner_radii` alongside
  its bounds, `Style::overflow_mask` fills them from the element's own corner
  radii, and every backend's fragment stage multiplies coverage by a rounded-rect
  SDF of the mask. Rectangular hardware clip planes still do the coarse cut, and
  a mask with zero radii takes an early out, so unrounded masks are unchanged.
  Implemented for quads, shadows, underlines, sprites, and paths on Metal, WGSL,
  and HLSL; hardware video `surface` primitives still clip rectangularly. Nested
  masks intersect corner by corner, so a corner the intersection did not cut
  keeps its rounding.
- `crates/gpui_base` is the `gpui-base` crate of
  [`longbridge/gpui-component`](https://github.com/longbridge/gpui-component)
  (Apache-2.0, copyright Longbridge), brought in with its history through the
  `vendor/gpui-component` branch. It builds against the GPUI crates of this repository instead of
  the `gpui-pre-*` snapshots upstream uses. Its changes from upstream: struct
  literals of `ContentMask` fill the new `corner_radii` field, small adaptations
  to GPUI API differences, and a `DockAreaRenderer::closed_dock_extent` hook that
  lets a dock renderer decide how much room a closed dock keeps. The root
  workspace takes `lsp-types` from crates.io for it instead of Zed's fork.
- Pen input was added: `PenEvent` (coalesced samples with pressure, tilt,
  rotation, timestamps, predicted and provisional samples), `PenGestureEvent`,
  `on_pen` / `capture_pen` listeners, and per-stroke claiming in `Window`, so a
  claimed stroke stays with its element and the platform does not turn it into
  touch or mouse input. Click handlers and scroll areas handle pen strokes. The
  iOS backend reads Apple Pencil touches, hover and pencil interactions. The
  Windows backend reads Windows Ink (`WM_POINTER*`) and Wintab, chosen with
  `App::set_pen_source` (`PenSource::Auto` prefers Wintab), disables the system
  pen gestures, and drops the pointer and mouse messages of claimed strokes.
  Wintab goes through the `wintab` crate. The macOS backend reads mouse events
  with tablet data and `NSEventTypeTabletPoint` events, tracks the pen in
  proximity with a local event monitor, and turns mouse coalescing off while
  the pen is in proximity. The pen example moved to `crates/gpui/examples/pen`
  and runs on Windows, macOS and iOS.
- On an auto-sized axis, absolute padding is snapped as a pair: the sum of both
  sides is rounded to device pixels and split by the authored ratio. Ported
  from [gpui-ce](https://github.com/gpui-ce/gpui-ce) #217 (Apache-2.0).
- Rounded corners can be smoothed toward a squircle: `Style::corner_smoothing`,
  `Styled::rounded_smoothing`, `PaintQuad::corner_smoothing`, and a
  `corner_smoothing` argument on `Window::paint_drop_shadows`,
  `paint_inset_shadows`, `paint_image` and `paint_image_wide`. Quads (fill,
  solid and dashed borders), shadows, polychrome sprites and rounded content
  masks all use the same Figma-style corner construction, so a smoothed element
  and the children it clips share one outline. `ContentMask` gained
  `corner_smoothing` and a padding word, which grows every primitive that
  carries a mask by two words. Ported from
  [gpui-ce](https://github.com/gpui-ce/gpui-ce) #235 and #338 (Apache-2.0); the
  corner geometry derives from figma-squircle (MIT), see
  [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

These changes are documented in detail, with code, rationale, and open
questions, in [docs/fork](docs/fork/README.md).

Further modifications will be tracked in this repository's git history.

## License

Apache License 2.0. See [LICENSE-APACHE](LICENSE-APACHE).

Copyright of the original code remains with Zed Industries, Inc. and the Zed
contributors, and for `crates/gpui_base` with Longbridge and the gpui-component
contributors. This fork contains no GPL-licensed code: every remaining crate is
licensed under Apache-2.0, and third-party dependencies are permissively
licensed (MIT/Apache/BSD/MPL-2.0 and similar, plus CDLA-Permissive-2.0 for the
`webpki-roots` / `webpki-root-certs` root certificate data).
The corner-smoothing shader code is derived from MIT-licensed figma-squircle;
see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

"Zed" and "GPUI" are trademarks of Zed Industries, Inc.; this fork is not
affiliated with or endorsed by Zed Industries.

## Upstream

The `upstream` remote tracks `zed-industries/zed`. Only GPUI-related changes are
merged from upstream. `crates/gpui_base` follows `longbridge/gpui-component`
through the `kit-upstream` remote. The merge workflow, conflict-resolution rules, and the
mandatory post-merge license audit (`script/check-licenses`) are documented in
[UPSTREAM.md](UPSTREAM.md).

```shell
cargo check -p gpui        # build the library
cargo run -p gpui --example hello_world
```
