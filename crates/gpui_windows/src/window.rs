#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    cell::{Cell, RefCell},
    mem::ManuallyDrop,
    num::NonZeroIsize,
    os::windows::ffi::OsStrExt,
    path::PathBuf,
    rc::{Rc, Weak},
    str::FromStr,
    sync::{Arc, Once, OnceLock, atomic::AtomicBool},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use futures::channel::oneshot::{self, Receiver};
use gpui_util::ResultExt;
use raw_window_handle as rwh;
use smallvec::SmallVec;
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Dwm::*,
        Graphics::Gdi::*,
        System::{
            Com::*,
            DataExchange::RegisterClipboardFormatW,
            Diagnostics::Debug::MessageBeep,
            LibraryLoader::*,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::*,
            SystemServices::*,
        },
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
    core::*,
};

use crate::direct_manipulation::DirectManipulationHandler;
use crate::*;
use gpui::*;

pub(crate) struct WindowsWindow(pub Rc<WindowsWindowInner>);

impl std::ops::Deref for WindowsWindow {
    type Target = WindowsWindowInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct WindowsWindowState {
    pub origin: Cell<Point<Pixels>>,
    pub logical_size: Cell<Size<Pixels>>,
    pub min_size: Option<Size<Pixels>>,
    pub fullscreen_restore_bounds: Cell<Bounds<Pixels>>,
    pub border_offset: WindowBorderOffset,
    pub appearance: Cell<WindowAppearance>,
    pub background_appearance: Cell<WindowBackgroundAppearance>,
    pub scale_factor: Cell<f32>,
    pub restore_from_minimized: Cell<Option<Box<dyn FnMut(RequestFrameOptions)>>>,

    pub callbacks: Callbacks,
    pub input_handler: Cell<Option<PlatformInputHandler>>,
    pub ime_enabled: Cell<bool>,
    pub pending_surrogate: Cell<Option<u16>>,
    pub last_reported_modifiers: Cell<Option<Modifiers>>,
    pub last_reported_capslock: Cell<Option<Capslock>>,
    pub hovered: Cell<bool>,
    pub direct_manipulation: DirectManipulationHandler,

    pub renderer: RefCell<DirectXRenderer>,
    /// Set when the next `draw_window` call must be treated as a forced
    /// render. Used after a GPU device-lost recovery, where the next frame
    /// must both re-enable drawing (via `mark_drawable`) and bypass the GPUI
    /// view cache (which would otherwise replay stale atlas tile references
    /// from the previous frame and panic in `DirectXAtlasState::texture`),
    /// and when a forced render was requested while another draw was in
    /// progress and had to be deferred.
    pub force_render_pending: Cell<bool>,

    pub click_state: ClickState,
    pub current_cursor: Cell<Option<HCURSOR>>,
    /// Shared with [`WindowsPlatformState::cursor_visible`].
    pub cursor_visible: Arc<AtomicBool>,
    pub nc_button_pressed: Cell<Option<u32>>,

    pub display: Cell<WindowsDisplay>,
    /// Flag to instruct the `VSyncProvider` thread to invalidate the directx devices
    /// as resizing them has failed, causing us to have lost at least the render target.
    pub invalidate_devices: Arc<AtomicBool>,
    /// Shared with [`WindowsPlatformState::draw_coordinator`] and every other window.
    pub(crate) draw_coordinator: Rc<DrawCoordinator>,
    fullscreen: Cell<Option<StyleAndBounds>>,
    initial_placement: Cell<Option<WindowOpenStatus>>,
    hwnd: HWND,
    pub(crate) a11y: RefCell<Option<A11yState>>,
    last_drag_session_cursor: Cell<Option<(i32, i32)>>,
}

pub(crate) struct WindowsWindowInner {
    hwnd: HWND,
    drop_target_helper: IDropTargetHelper,
    pub(crate) state: WindowsWindowState,
    system_settings: WindowsSystemSettings,
    pub(crate) handle: AnyWindowHandle,
    pub(crate) kind: WindowKind,
    pub(crate) hide_title_bar: bool,
    pub(crate) is_movable: bool,
    pub(crate) is_resizable: bool,
    pub(crate) is_minimizable: bool,
    pub(crate) executor: ForegroundExecutor,
    pub(crate) validation_number: usize,
    pub(crate) main_receiver: PriorityQueueReceiver<RunnableVariant>,
    pub(crate) platform_window_handle: HWND,
    pub(crate) parent_hwnd: Option<HWND>,
}

impl WindowsWindowState {
    fn new(
        hwnd: HWND,
        directx_devices: &DirectXDevices,
        window_params: &CREATESTRUCTW,
        current_cursor: Option<HCURSOR>,
        cursor_visible: Arc<AtomicBool>,
        display: WindowsDisplay,
        min_size: Option<Size<Pixels>>,
        appearance: WindowAppearance,
        disable_direct_composition: bool,
        invalidate_devices: Arc<AtomicBool>,
        draw_coordinator: Rc<DrawCoordinator>,
    ) -> Result<Self> {
        let scale_factor = {
            let monitor_dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
            monitor_dpi / USER_DEFAULT_SCREEN_DPI as f32
        };
        let origin = logical_point(window_params.x as f32, window_params.y as f32, scale_factor);
        let logical_size = {
            let physical_size = size(
                DevicePixels(window_params.cx),
                DevicePixels(window_params.cy),
            );
            physical_size.to_pixels(scale_factor)
        };
        let fullscreen_restore_bounds = Bounds {
            origin,
            size: logical_size,
        };
        let border_offset = WindowBorderOffset::default();
        let restore_from_minimized = None;
        let renderer = DirectXRenderer::new(hwnd, directx_devices, disable_direct_composition)
            .context("Creating DirectX renderer")?;
        let callbacks = Callbacks::default();
        let input_handler = None;
        let pending_surrogate = None;
        let last_reported_modifiers = None;
        let last_reported_capslock = None;
        let hovered = false;
        let click_state = ClickState::new();
        let nc_button_pressed = None;
        let fullscreen = None;
        let initial_placement = None;

        let direct_manipulation = DirectManipulationHandler::new(hwnd, scale_factor)
            .context("initializing Direct Manipulation")?;

        Ok(Self {
            origin: Cell::new(origin),
            logical_size: Cell::new(logical_size),
            fullscreen_restore_bounds: Cell::new(fullscreen_restore_bounds),
            border_offset,
            appearance: Cell::new(appearance),
            background_appearance: Cell::new(WindowBackgroundAppearance::Opaque),
            scale_factor: Cell::new(scale_factor),
            restore_from_minimized: Cell::new(restore_from_minimized),
            min_size,
            callbacks,
            input_handler: Cell::new(input_handler),
            ime_enabled: Cell::new(true),
            pending_surrogate: Cell::new(pending_surrogate),
            last_reported_modifiers: Cell::new(last_reported_modifiers),
            last_reported_capslock: Cell::new(last_reported_capslock),
            hovered: Cell::new(hovered),
            renderer: RefCell::new(renderer),
            force_render_pending: Cell::new(false),
            click_state,
            current_cursor: Cell::new(current_cursor),
            cursor_visible,
            nc_button_pressed: Cell::new(nc_button_pressed),
            display: Cell::new(display),
            fullscreen: Cell::new(fullscreen),
            initial_placement: Cell::new(initial_placement),
            hwnd,
            invalidate_devices,
            draw_coordinator,
            direct_manipulation,
            a11y: RefCell::new(None),
            last_drag_session_cursor: Cell::new(None),
        })
    }

    #[inline]
    pub(crate) fn is_fullscreen(&self) -> bool {
        self.fullscreen.get().is_some()
    }

    pub(crate) fn is_maximized(&self) -> bool {
        !self.is_fullscreen() && unsafe { IsZoomed(self.hwnd) }.as_bool()
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: self.origin.get(),
            size: self.logical_size.get(),
        }
    }

    // Calculate the bounds used for saving and whether the window is maximized.
    fn calculate_window_bounds(&self) -> (Bounds<Pixels>, bool) {
        let placement = unsafe {
            let mut placement = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            GetWindowPlacement(self.hwnd, &mut placement)
                .context("failed to get window placement")
                .log_err();
            placement
        };
        (
            calculate_client_rect(
                placement.rcNormalPosition,
                &self.border_offset,
                self.scale_factor.get(),
            ),
            placement.showCmd == SW_SHOWMAXIMIZED.0 as u32,
        )
    }

    fn window_bounds(&self) -> WindowBounds {
        let (bounds, maximized) = self.calculate_window_bounds();

        if self.is_fullscreen() {
            WindowBounds::Fullscreen(self.fullscreen_restore_bounds.get())
        } else if maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        }
    }

    /// get the logical size of the app's drawable area.
    ///
    /// Currently, GPUI uses the logical size of the app to handle mouse interactions (such as
    /// whether the mouse collides with other elements of GPUI).
    fn content_size(&self) -> Size<Pixels> {
        self.logical_size.get()
    }
}

impl WindowsWindowInner {
    fn new(context: &mut WindowCreateContext, hwnd: HWND, cs: &CREATESTRUCTW) -> Result<Rc<Self>> {
        let state = WindowsWindowState::new(
            hwnd,
            &context.directx_devices,
            cs,
            context.current_cursor,
            context.cursor_visible.clone(),
            context.display,
            context.min_size,
            context.appearance,
            context.disable_direct_composition,
            context.invalidate_devices.clone(),
            context.draw_coordinator.clone(),
        )?;

        Ok(Rc::new(Self {
            hwnd,
            drop_target_helper: context.drop_target_helper.clone(),
            state,
            handle: context.handle,
            kind: context.kind.clone(),
            hide_title_bar: context.hide_title_bar,
            is_movable: context.is_movable,
            is_resizable: context.is_resizable,
            is_minimizable: context.is_minimizable,
            executor: context.executor.clone(),
            validation_number: context.validation_number,
            main_receiver: context.main_receiver.clone(),
            platform_window_handle: context.platform_window_handle,
            system_settings: WindowsSystemSettings::new(),
            parent_hwnd: context.parent_hwnd,
        }))
    }

    fn toggle_fullscreen(self: &Rc<Self>) {
        let this = self.clone();
        self.executor
            .spawn(async move {
                let StyleAndBounds {
                    style,
                    x,
                    y,
                    cx,
                    cy,
                } = match this.state.fullscreen.take() {
                    Some(state) => state,
                    None => {
                        let (window_bounds, _) = this.state.calculate_window_bounds();
                        this.state.fullscreen_restore_bounds.set(window_bounds);

                        let style =
                            WINDOW_STYLE(unsafe { get_window_long(this.hwnd, GWL_STYLE) } as _);
                        let mut rc = RECT::default();
                        unsafe { GetWindowRect(this.hwnd, &mut rc) }
                            .context("failed to get window rect")
                            .log_err();
                        let _ = this.state.fullscreen.set(Some(StyleAndBounds {
                            style,
                            x: rc.left,
                            y: rc.top,
                            cx: rc.right - rc.left,
                            cy: rc.bottom - rc.top,
                        }));
                        let style = style
                            & !(WS_THICKFRAME
                                | WS_SYSMENU
                                | WS_MAXIMIZEBOX
                                | WS_MINIMIZEBOX
                                | WS_CAPTION);
                        let physical_bounds = this.state.display.get().physical_bounds();
                        StyleAndBounds {
                            style,
                            x: physical_bounds.left().0,
                            y: physical_bounds.top().0,
                            cx: physical_bounds.size.width.0,
                            cy: physical_bounds.size.height.0,
                        }
                    }
                };
                set_non_rude_hwnd(this.hwnd, !this.state.is_fullscreen());
                unsafe { set_window_long(this.hwnd, GWL_STYLE, style.0 as isize) };
                unsafe {
                    SetWindowPos(
                        this.hwnd,
                        None,
                        x,
                        y,
                        cx,
                        cy,
                        SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER,
                    )
                }
                .log_err();
            })
            .detach();
    }

    fn set_window_placement(self: &Rc<Self>) -> Result<()> {
        let Some(open_status) = self.state.initial_placement.take() else {
            return Ok(());
        };
        // The app has shown the window itself since, so where it put it wins.
        if unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            return Ok(());
        }
        match open_status.state {
            WindowOpenState::Maximized => unsafe {
                SetWindowPlacement(self.hwnd, &open_status.placement)
                    .context("failed to set window placement")?;
                ShowWindowAsync(self.hwnd, SW_MAXIMIZE).ok()?;
            },
            WindowOpenState::Fullscreen => {
                unsafe {
                    SetWindowPlacement(self.hwnd, &open_status.placement)
                        .context("failed to set window placement")?
                };
                self.toggle_fullscreen();
            }
            WindowOpenState::Windowed => unsafe {
                SetWindowPlacement(self.hwnd, &open_status.placement)
                    .context("failed to set window placement")?;
            },
        }
        Ok(())
    }

    pub(crate) fn system_settings(&self) -> &WindowsSystemSettings {
        &self.system_settings
    }

    fn dispatch_drag_input(&self, input: PlatformInput) {
        if let Some(mut func) = self.state.callbacks.input.take() {
            func(input);
            self.state.callbacks.input.set(Some(func));
        }
    }

    /// `IDropSource` callbacks carry no position, so it is read from the cursor.
    fn emit_drag_session_moved(&self) {
        let mut cursor = POINT::default();
        unsafe {
            if GetCursorPos(&mut cursor).log_err().is_none() {
                return;
            }
        }
        if self.state.last_drag_session_cursor.get() == Some((cursor.x, cursor.y)) {
            return;
        }
        self.state
            .last_drag_session_cursor
            .set(Some((cursor.x, cursor.y)));

        let mut client_position = cursor;
        unsafe {
            ScreenToClient(self.hwnd, &mut client_position).ok().log_err();
        }
        let position = logical_point(
            client_position.x as f32,
            client_position.y as f32,
            self.state.scale_factor.get(),
        );
        self.dispatch_drag_input(PlatformInput::FileDrop(FileDropEvent::SessionMoved {
            position,
        }));
    }

    /// A Win32 modal loop owns the thread and never reaches the main loop's task
    /// processing, so without this a drag handler's work replays after the drop.
    fn run_pending_foreground_tasks(&self) {
        let mut runnables = self.main_receiver.clone().try_iter();
        while let Some(Ok(runnable)) = runnables.next() {
            WindowsDispatcher::execute_runnable(runnable);
        }
    }
}

#[derive(Default)]
pub(crate) struct Callbacks {
    pub(crate) request_frame: Cell<Option<Box<dyn FnMut(RequestFrameOptions)>>>,
    pub(crate) input: Cell<Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>>,
    pub(crate) active_status_change: Cell<Option<Box<dyn FnMut(bool)>>>,
    pub(crate) hovered_status_change: Cell<Option<Box<dyn FnMut(bool)>>>,
    pub(crate) resize: Cell<Option<Box<dyn FnMut(Size<Pixels>, f32)>>>,
    pub(crate) moved: Cell<Option<Box<dyn FnMut()>>>,
    pub(crate) move_loop_ended: Cell<Option<Box<dyn FnMut()>>>,
    pub(crate) should_close: Cell<Option<Box<dyn FnMut() -> bool>>>,
    pub(crate) close: Cell<Option<Box<dyn FnOnce()>>>,
    pub(crate) hit_test_window_control: Cell<Option<Box<dyn FnMut() -> Option<WindowControlArea>>>>,
    pub(crate) appearance_changed: Cell<Option<Box<dyn FnMut()>>>,
}

struct WindowCreateContext {
    inner: Option<Result<Rc<WindowsWindowInner>>>,
    handle: AnyWindowHandle,
    kind: WindowKind,
    hide_title_bar: bool,
    display: WindowsDisplay,
    is_movable: bool,
    is_resizable: bool,
    is_minimizable: bool,
    min_size: Option<Size<Pixels>>,
    executor: ForegroundExecutor,
    current_cursor: Option<HCURSOR>,
    cursor_visible: Arc<AtomicBool>,
    drop_target_helper: IDropTargetHelper,
    validation_number: usize,
    main_receiver: PriorityQueueReceiver<RunnableVariant>,
    platform_window_handle: HWND,
    appearance: WindowAppearance,
    disable_direct_composition: bool,
    directx_devices: DirectXDevices,
    invalidate_devices: Arc<AtomicBool>,
    draw_coordinator: Rc<DrawCoordinator>,
    parent_hwnd: Option<HWND>,
}

impl WindowsWindow {
    pub(crate) fn new(
        handle: AnyWindowHandle,
        params: WindowParams,
        creation_info: WindowCreationInfo,
    ) -> Result<Self> {
        // Native popups are not implemented on Windows yet. Rejecting lets callers fall back to
        // gpui's in-window popovers.
        if let WindowKind::AnchoredPopup(_) = params.kind {
            return Err(popup::PopupNotSupportedError.into());
        }

        let WindowCreationInfo {
            icon,
            executor,
            current_cursor,
            cursor_visible,
            drop_target_helper,
            validation_number,
            main_receiver,
            platform_window_handle,
            disable_direct_composition,
            directx_devices,
            invalidate_devices,
            draw_coordinator,
        } = creation_info;
        register_window_class(icon);
        let parent_hwnd = if params.kind == WindowKind::Dialog {
            let parent_window = unsafe { GetActiveWindow() };
            if parent_window.is_invalid() {
                None
            } else {
                // Disable the parent window to make this dialog modal
                unsafe {
                    EnableWindow(parent_window, false).as_bool();
                };
                Some(parent_window)
            }
        } else {
            None
        };
        let hide_title_bar = params
            .titlebar
            .as_ref()
            .map(|titlebar| titlebar.appears_transparent)
            .unwrap_or(true);
        let window_name = HSTRING::from(
            params
                .titlebar
                .as_ref()
                .and_then(|titlebar| titlebar.title.as_ref())
                .map(|title| title.as_ref())
                .unwrap_or(""),
        );

        let (mut dwexstyle, dwstyle) = if params.kind == WindowKind::PopUp
            || params.kind == WindowKind::Overlay
        {
            let mut dwexstyle = WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
            if params.kind == WindowKind::Overlay {
                dwexstyle |= WS_EX_NOACTIVATE | WS_EX_TRANSPARENT;
            }
            (dwexstyle, WINDOW_STYLE(0x0))
        } else {
            let mut dwstyle = WS_SYSMENU;

            if params.is_resizable {
                dwstyle |= WS_THICKFRAME | WS_MAXIMIZEBOX;
            }

            if params.is_minimizable {
                dwstyle |= WS_MINIMIZEBOX;
            }
            let dwexstyle = if params.kind == WindowKind::Dialog {
                dwstyle |= WS_POPUP | WS_CAPTION;
                WS_EX_DLGMODALFRAME
            } else {
                WS_EX_APPWINDOW
            };

            (dwexstyle, dwstyle)
        };
        if !disable_direct_composition {
            dwexstyle |= WS_EX_NOREDIRECTIONBITMAP;
        }

        let hinstance = get_module_handle();
        let display = if let Some(display_id) = params.display_id {
            WindowsDisplay::new(display_id)
        } else {
            None
        }
        .or_else(WindowsDisplay::primary_monitor)
        .context("failed to find any monitor")?;
        let appearance = system_appearance().unwrap_or_default();
        let mut context = WindowCreateContext {
            inner: None,
            handle,
            kind: params.kind.clone(),
            hide_title_bar,
            display,
            is_movable: params.is_movable,
            is_resizable: params.is_resizable,
            is_minimizable: params.is_minimizable,
            min_size: params.window_min_size,
            executor,
            current_cursor,
            cursor_visible,
            drop_target_helper,
            validation_number,
            main_receiver,
            platform_window_handle,
            appearance,
            disable_direct_composition,
            directx_devices,
            invalidate_devices,
            draw_coordinator,
            parent_hwnd,
        };
        let creation_result = unsafe {
            CreateWindowExW(
                dwexstyle,
                WINDOW_CLASS_NAME,
                &window_name,
                dwstyle,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                parent_hwnd,
                None,
                Some(hinstance.into()),
                Some(&context as *const _ as *const _),
            )
        };

        // Failure to create a `WindowsWindowState` can cause window creation to fail,
        // so check the inner result first.
        let this = context.inner.take().transpose()?;
        let hwnd = creation_result?;
        let this = this.unwrap();

        // An overlay may sit directly under the cursor, so both drag routing
        // and hit-testing must pass to the window below.
        if params.kind != WindowKind::Overlay {
            register_drag_drop(&this)?;
        } else {
            unsafe {
                let policy = DWMNCRP_DISABLED;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_NCRENDERING_POLICY,
                    &policy as *const _ as _,
                    std::mem::size_of::<DWMNCRENDERINGPOLICY>() as u32,
                )
                .log_err();

                // An overlay must track the cursor from its first frame, so
                // DWM's open animation reads as latency.
                let disabled: BOOL = true.into();
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_TRANSITIONS_FORCEDISABLED,
                    &disabled as *const _ as _,
                    std::mem::size_of::<BOOL>() as u32,
                )
                .log_err();
            }
        }
        set_non_rude_hwnd(hwnd, true);
        configure_dwm_dark_mode(hwnd, appearance);
        this.state.border_offset.update(hwnd)?;
        let app_placed_window = params.kind == WindowKind::Overlay || !params.focus;
        let placement = retrieve_window_placement(
            hwnd,
            display,
            params.bounds,
            this.state.scale_factor.get(),
            &this.state.border_offset,
            // The fallback is for restored bounds, and a restore always takes
            // focus. A window the app places may sit clear of every monitor.
            app_placed_window,
        )?;
        if params.show {
            let mut placement = placement;
            if !params.focus {
                placement.showCmd = SW_SHOWNOACTIVATE.0 as u32;
            }
            // `SetWindowPlacement` restores a window onto a monitor, dragging
            // an off-display origin back into view. `SetWindowPos` does not.
            if app_placed_window {
                let rect = placement.rcNormalPosition;
                unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                    .context("unable to place window")?;
                    // `ShowWindowAsync` reports the *previous* visibility, so a
                    // successful first show returns false.
                    let _ = ShowWindowAsync(
                        hwnd,
                        if params.focus {
                            SW_SHOWNORMAL
                        } else {
                            SW_SHOWNOACTIVATE
                        },
                    );
                }
            } else {
                unsafe { SetWindowPlacement(hwnd, &placement)? };
            }
        } else {
            this.state.initial_placement.set(Some(WindowOpenStatus {
                placement,
                state: WindowOpenState::Windowed,
            }));
        }

        Ok(Self(this))
    }
}

impl rwh::HasWindowHandle for WindowsWindow {
    fn window_handle(&self) -> std::result::Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let raw = rwh::Win32WindowHandle::new(unsafe {
            NonZeroIsize::new_unchecked(self.0.hwnd.0 as isize)
        })
        .into();
        Ok(unsafe { rwh::WindowHandle::borrow_raw(raw) })
    }
}

impl rwh::HasDisplayHandle for WindowsWindow {
    fn display_handle(&self) -> std::result::Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        Ok(rwh::DisplayHandle::windows())
    }
}

impl Drop for WindowsWindow {
    fn drop(&mut self) {
        // clone this `Rc` to prevent early release of the pointer
        let this = self.0.clone();
        self.0
            .executor
            .spawn(async move {
                let handle = this.hwnd;
                unsafe {
                    let _ = RevokeDragDrop(handle);
                    DestroyWindow(handle).log_err();
                }
            })
            .detach();
    }
}

impl PlatformWindow for WindowsWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.state.bounds()
    }

    fn is_maximized(&self) -> bool {
        self.state.is_maximized()
    }

    fn window_bounds(&self) -> WindowBounds {
        self.state.window_bounds()
    }

    /// get the logical size of the app's drawable area.
    ///
    /// Currently, GPUI uses the logical size of the app to handle mouse interactions (such as
    /// whether the mouse collides with other elements of GPUI).
    fn content_size(&self) -> Size<Pixels> {
        self.state.content_size()
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let hwnd = self.0.hwnd;
        let bounds = gpui::bounds(self.bounds().origin, size).to_device_pixels(self.scale_factor());
        let rect = calculate_window_rect(bounds, &self.state.border_offset);

        self.0
            .executor
            .spawn(async move {
                unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        bounds.origin.x.0,
                        bounds.origin.y.0,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOMOVE,
                    )
                    .context("unable to set window content size")
                    .log_err();
                }
            })
            .detach();
    }

    fn scale_factor(&self) -> f32 {
        self.state.scale_factor.get()
    }

    fn appearance(&self) -> WindowAppearance {
        self.state.appearance.get()
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(self.state.display.get()))
    }

    fn mouse_position(&self) -> Point<Pixels> {
        let scale_factor = self.scale_factor();
        let point = unsafe {
            let mut point: POINT = std::mem::zeroed();
            GetCursorPos(&mut point)
                .context("unable to get cursor position")
                .log_err();
            ScreenToClient(self.0.hwnd, &mut point).ok().log_err();
            point
        };
        logical_point(point.x as f32, point.y as f32, scale_factor)
    }

    fn modifiers(&self) -> Modifiers {
        current_modifiers()
    }

    fn capslock(&self) -> Capslock {
        current_capslock()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.state.input_handler.set(Some(input_handler));
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.state.input_handler.take()
    }

    fn move_to(&mut self, origin: Point<Pixels>) {
        let this = self.0.clone();
        let scale_factor = self.scale_factor();
        self.0
            .executor
            .spawn(async move {
                let target_x = (origin.x.as_f32() * scale_factor).round() as i32;
                let target_y = (origin.y.as_f32() * scale_factor).round() as i32;
                unsafe {
                    let mut window_rect = RECT::default();
                    if GetWindowRect(this.hwnd, &mut window_rect).log_err().is_none() {
                        return;
                    }
                    // `WM_MOVE` reports the client area but `SetWindowPos`
                    // places the window rect; see pitfalls 20.
                    let mut client_origin = POINT::default();
                    if !ClientToScreen(this.hwnd, &mut client_origin).as_bool() {
                        return;
                    }
                    let x = target_x - (client_origin.x - window_rect.left);
                    let y = target_y - (client_origin.y - window_rect.top);
                    SetWindowPos(
                        this.hwnd,
                        None,
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                    .log_err();
                }
            })
            .detach();
    }

    fn set_accepts_drags(&self, accepts: bool) {
        // Applied synchronously: a window opting out mid-gesture has to stop
        // being a destination before the next dragging event arrives.
        if self.0.kind == WindowKind::Overlay {
            return;
        }
        unsafe {
            let _ = RevokeDragDrop(self.0.hwnd);
        }
        if accepts {
            register_drag_drop(&self.0).log_err();
        }
    }

    fn can_start_external_drag(&self) -> bool {
        true
    }

    fn start_external_drag(&self, payload: &ExternalDragPayload) -> bool {
        let data_object = match payload {
            ExternalDragPayload::Files(paths) => {
                if paths.entries().is_empty() {
                    log::warn!("start_external_drag declined: no paths");
                    return false;
                }
                file_drag_data_object(paths)
            }
            ExternalDragPayload::AppPrivate => private_drag_data_object(),
        };
        let Some(data_object) = data_object else {
            log::warn!("start_external_drag declined: failed to build data object");
            return false;
        };
        let use_default_cursors = matches!(payload, ExternalDragPayload::Files(_));
        let this = self.0.clone();
        // `DoDragDrop` runs a modal loop until the drag ends, so it is deferred
        // out of the input dispatch that initiated it.
        self.0
            .executor
            .spawn(async move {
                let drop_source: IDropSource = WindowsDragSource {
                    window: this.clone(),
                    use_default_cursors,
                }
                .into();
                this.state.last_drag_session_cursor.set(None);
                let mut effect = DROPEFFECT_NONE;
                let result = unsafe {
                    DoDragDrop(
                        &data_object,
                        &drop_source,
                        DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
                        &mut effect,
                    )
                };
                log::info!("external drag session ended: {result:?}");
                this.dispatch_drag_input(PlatformInput::FileDrop(FileDropEvent::Ended));
            })
            .detach();
        true
    }

    fn prompt(
        &self,
        level: PromptLevel,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<Receiver<usize>> {
        let (done_tx, done_rx) = oneshot::channel();
        let msg = msg.to_string();
        let detail_string = detail.map(|detail| detail.to_string());
        let handle = self.0.hwnd;
        let answers = answers.to_vec();
        self.0
            .executor
            .spawn(async move {
                unsafe {
                    let mut config = TASKDIALOGCONFIG::default();
                    config.cbSize = std::mem::size_of::<TASKDIALOGCONFIG>() as _;
                    config.hwndParent = handle;
                    let title;
                    let main_icon;
                    match level {
                        PromptLevel::Info => {
                            title = windows::core::w!("Info");
                            main_icon = TD_INFORMATION_ICON;
                        }
                        PromptLevel::Warning => {
                            title = windows::core::w!("Warning");
                            main_icon = TD_WARNING_ICON;
                        }
                        PromptLevel::Critical => {
                            title = windows::core::w!("Critical");
                            main_icon = TD_ERROR_ICON;
                        }
                    };
                    config.pszWindowTitle = title;
                    config.Anonymous1.pszMainIcon = main_icon;
                    let instruction = HSTRING::from(msg);
                    config.pszMainInstruction = PCWSTR::from_raw(instruction.as_ptr());
                    let hints_encoded;
                    if let Some(ref hints) = detail_string {
                        hints_encoded = HSTRING::from(hints);
                        config.pszContent = PCWSTR::from_raw(hints_encoded.as_ptr());
                    };
                    let mut button_id_map = Vec::with_capacity(answers.len());
                    let mut buttons = Vec::new();
                    let mut btn_encoded = Vec::new();
                    for (index, btn) in answers.iter().enumerate() {
                        let encoded = HSTRING::from(btn.label().as_ref());
                        let button_id = match btn {
                            PromptButton::Ok(_) => IDOK.0,
                            PromptButton::Cancel(_) => IDCANCEL.0,
                            // the first few low integer values are reserved for known buttons
                            // so for simplicity we just go backwards from -1
                            PromptButton::Other(_) => -(index as i32) - 1,
                        };
                        button_id_map.push(button_id);
                        buttons.push(TASKDIALOG_BUTTON {
                            nButtonID: button_id,
                            pszButtonText: PCWSTR::from_raw(encoded.as_ptr()),
                        });
                        btn_encoded.push(encoded);
                    }
                    config.cButtons = buttons.len() as _;
                    config.pButtons = buttons.as_ptr();

                    config.pfCallback = None;
                    let mut res = std::mem::zeroed();
                    let _ = TaskDialogIndirect(&config, Some(&mut res), None, None)
                        .context("unable to create task dialog")
                        .log_err();

                    if let Some(clicked) =
                        button_id_map.iter().position(|&button_id| button_id == res)
                    {
                        let _ = done_tx.send(clicked);
                    }
                }
            })
            .detach();

        Some(done_rx)
    }

    fn activate(&self) {
        let hwnd = self.0.hwnd;
        let this = self.0.clone();
        self.0
            .executor
            .spawn(async move {
                this.set_window_placement().log_err();

                unsafe {
                    // If the window is minimized, restore it.
                    if IsIconic(hwnd).as_bool() {
                        ShowWindowAsync(hwnd, SW_RESTORE).ok().log_err();
                    }

                    SetActiveWindow(hwnd).ok();
                    SetFocus(Some(hwnd)).ok();
                }

                // premium ragebait by windows, this is needed because the window
                // must have received an input event to be able to set itself to foreground
                // so let's just simulate user input as that seems to be the most reliable way
                // some more info: https://gist.github.com/Aetopia/1581b40f00cc0cadc93a0e8ccb65dc8c
                // bonus: this bug also doesn't manifest if you have vs attached to the process
                let inputs = [
                    INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: VK_MENU,
                                dwFlags: KEYBD_EVENT_FLAGS(0),
                                ..Default::default()
                            },
                        },
                    },
                    INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: VK_MENU,
                                dwFlags: KEYEVENTF_KEYUP,
                                ..Default::default()
                            },
                        },
                    },
                ];
                unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };

                // todo(windows)
                // crate `windows 0.56` reports true as Err
                unsafe { SetForegroundWindow(hwnd).as_bool() };
            })
            .detach();
    }

    fn request_attention(&self) {
        if self.is_active() {
            return;
        }

        let hwnd = self.0.hwnd;
        self.0
            .executor
            .spawn(async move {
                let info = FLASHWINFO {
                    cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                    hwnd,
                    dwFlags: FLASHW_ALL,
                    uCount: 1,
                    dwTimeout: 0,
                };
                unsafe { FlashWindowEx(&info).ok().log_err() };
            })
            .detach();
    }

    fn is_active(&self) -> bool {
        self.0.hwnd == unsafe { GetActiveWindow() }
    }

    fn is_hovered(&self) -> bool {
        self.state.hovered.get()
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        self.state.background_appearance.get()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        true
    }

    fn set_title(&mut self, title: &str) {
        unsafe { SetWindowTextW(self.0.hwnd, &HSTRING::from(title)) }
            .inspect_err(|e| log::error!("Set title failed: {e}"))
            .ok();
    }

    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        self.state.background_appearance.set(background_appearance);
        let hwnd = self.0.hwnd;

        // using Dwm APIs for Mica and MicaAlt backdrops.
        // others follow the set_window_composition_attribute approach
        match background_appearance {
            WindowBackgroundAppearance::Opaque => {
                set_window_composition_attribute(hwnd, None, 0);
            }
            WindowBackgroundAppearance::Transparent => {
                set_window_composition_attribute(hwnd, None, 2);
            }
            WindowBackgroundAppearance::Blurred => {
                set_window_composition_attribute(hwnd, Some((0, 0, 0, 0)), 4);
            }
            WindowBackgroundAppearance::MicaBackdrop => {
                // DWMSBT_MAINWINDOW => MicaBase
                dwm_set_window_composition_attribute(hwnd, 2);
            }
            WindowBackgroundAppearance::MicaAltBackdrop => {
                // DWMSBT_TABBEDWINDOW => MicaAlt
                dwm_set_window_composition_attribute(hwnd, 4);
            }
        }
    }

    fn minimize(&self) {
        unsafe { ShowWindowAsync(self.0.hwnd, SW_MINIMIZE).ok().log_err() };
    }

    fn start_window_move(&self) {
        let hwnd = self.0.hwnd;
        // `SC_MOVE` runs a modal loop until the button is released, so like
        // `start_external_drag` it is deferred out of the input dispatch.
        self.0
            .executor
            .spawn(async move {
                unsafe {
                    ReleaseCapture().log_err();
                    SendMessageW(
                        hwnd,
                        WM_SYSCOMMAND,
                        Some(WPARAM((SC_MOVE | 0x0002) as usize)),
                        Some(LPARAM(GetMessagePos() as isize)),
                    );
                }
            })
            .detach();
    }

    fn zoom(&self) {
        unsafe {
            if IsWindowVisible(self.0.hwnd).as_bool() {
                ShowWindowAsync(self.0.hwnd, SW_MAXIMIZE).ok().log_err();
            } else if let Some(mut status) = self.state.initial_placement.take() {
                status.state = WindowOpenState::Maximized;
                self.state.initial_placement.set(Some(status));
            }
        }
    }

    fn toggle_fullscreen(&self) {
        if unsafe { IsWindowVisible(self.0.hwnd).as_bool() } {
            self.0.toggle_fullscreen();
        } else if let Some(mut status) = self.state.initial_placement.take() {
            status.state = WindowOpenState::Fullscreen;
            self.state.initial_placement.set(Some(status));
        }
    }

    fn is_fullscreen(&self) -> bool {
        self.state.is_fullscreen()
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.state.callbacks.request_frame.set(Some(callback));
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.state.callbacks.input.set(Some(callback));
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0
            .state
            .callbacks
            .active_status_change
            .set(Some(callback));
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0
            .state
            .callbacks
            .hovered_status_change
            .set(Some(callback));
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.state.callbacks.resize.set(Some(callback));
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.state.callbacks.moved.set(Some(callback));
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.state.callbacks.should_close.set(Some(callback));
    }

    fn on_move_loop_ended(&self, callback: Box<dyn FnMut()>) {
        self.state.callbacks.move_loop_ended.set(Some(callback));
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.state.callbacks.close.set(Some(callback));
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.0
            .state
            .callbacks
            .hit_test_window_control
            .set(Some(callback));
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0
            .state
            .callbacks
            .appearance_changed
            .set(Some(callback));
    }

    fn draw(&self, scene: &Scene) {
        self.state
            .renderer
            .borrow_mut()
            .draw(scene, self.state.background_appearance.get())
            .log_err();
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.state.renderer.borrow().sprite_atlas()
    }

    fn get_raw_handle(&self) -> HWND {
        self.0.hwnd
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.state.renderer.borrow().gpu_specs().log_err()
    }

    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        let scale_factor = self.state.scale_factor.get();
        let caret_position = POINT {
            x: (bounds.origin.x.as_f32() * scale_factor) as i32,
            y: (bounds.origin.y.as_f32() * scale_factor) as i32
                + ((bounds.size.height.as_f32() * scale_factor) as i32 / 2),
        };

        self.0.update_ime_position(self.0.hwnd, caret_position);
    }

    fn play_system_bell(&self) {
        // MB_OK: The sound specified as the Windows Default Beep sound.
        let _ = unsafe { MessageBeep(MB_OK) };
    }

    fn a11y_init(&self, callbacks: gpui::A11yCallbacks) {
        let action_handler = A11yActionHandler(callbacks.action);
        let is_focused = unsafe { GetForegroundWindow() } == self.0.hwnd;

        let adapter = accesskit_windows::Adapter::new(
            accesskit_windows::HWND(self.0.hwnd.0),
            is_focused,
            action_handler,
        );

        let activation_handler = A11yActivationHandler {
            callback: callbacks.activation,
        };

        *self.state.a11y.borrow_mut() = Some(A11yState {
            adapter,
            activation_handler,
        });
    }

    fn a11y_tree_update(&self, tree_update: accesskit::TreeUpdate) {
        let events = {
            let mut a11y = self.state.a11y.borrow_mut();
            a11y.as_mut()
                .and_then(|a11y| a11y.adapter.update_if_active(|| tree_update))
        };
        // The borrow must be dropped before raising events, because
        // `events.raise()` calls `UiaRaiseAutomationPropertyChangedEvent`
        // which may send a nested `WM_GETOBJECT` back into this window
        // procedure, re-entering `handle_wm_getobject` which also borrows
        // `self.state.a11y`.
        if let Some(events) = events {
            events.raise();
        }
    }

    fn a11y_update_window_bounds(&self) {
        // Windows UIA handles window bounds tracking automatically.
    }
}

pub(crate) struct A11yState {
    pub(crate) adapter: accesskit_windows::Adapter,
    pub(crate) activation_handler: A11yActivationHandler,
}

pub(crate) struct A11yActivationHandler {
    callback: Box<dyn Fn() -> Option<accesskit::TreeUpdate> + Send + 'static>,
}

impl accesskit::ActivationHandler for A11yActivationHandler {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        (self.callback)()
    }
}

struct A11yActionHandler(Box<dyn Fn(accesskit::ActionRequest) + Send + 'static>);

impl accesskit::ActionHandler for A11yActionHandler {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        (self.0)(request);
    }
}

// Per-app, so that two apps built on this gpui do not accept each other's
// private drags as empty file drops.
fn private_drag_format() -> Option<u16> {
    static FORMAT: OnceLock<Option<u16>> = OnceLock::new();
    *FORMAT.get_or_init(|| {
        let identity = std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let name = HSTRING::from(format!("app.gpui.private-drag.{identity}"));
        let format = unsafe { RegisterClipboardFormatW(&name) };
        if format == 0 {
            log::error!(
                "unable to register the app-private drag clipboard format: {}",
                std::io::Error::last_os_error()
            );
            return None;
        }
        Some(format as u16)
    })
}

fn hglobal_format(format: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: format,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn is_private_drag(data_object: &IDataObject) -> bool {
    let Some(format) = private_drag_format() else {
        return false;
    };
    unsafe { data_object.QueryGetData(&hglobal_format(format)) == S_OK }
}

fn private_drag_data_object() -> Option<IDataObject> {
    // The payload is a placeholder; the format itself is the identification.
    let format = private_drag_format()?;
    Some(
        WindowsDragDataObject {
            formats: vec![(format, b"gpui".to_vec())],
        }
        .into(),
    )
}

fn file_drag_data_object(paths: &FileDragPaths) -> Option<IDataObject> {
    let mut names = Vec::new();
    for (path, _) in paths.entries() {
        names.extend(dunce::simplified(path).as_os_str().encode_wide());
        names.push(0);
    }
    names.push(0);

    let header = DROPFILES {
        pFiles: std::mem::size_of::<DROPFILES>() as u32,
        pt: POINT::default(),
        fNC: false.into(),
        fWide: true.into(),
    };
    let mut bytes = Vec::with_capacity(std::mem::size_of::<DROPFILES>() + names.len() * 2);
    bytes.extend_from_slice(unsafe {
        std::slice::from_raw_parts(
            (&header as *const DROPFILES).cast::<u8>(),
            std::mem::size_of::<DROPFILES>(),
        )
    });
    for unit in names {
        bytes.extend_from_slice(&unit.to_ne_bytes());
    }

    Some(
        WindowsDragDataObject {
            formats: vec![(CF_HDROP.0, bytes)],
        }
        .into(),
    )
}

#[implement(IDataObject)]
struct WindowsDragDataObject {
    formats: Vec<(u16, Vec<u8>)>,
}

impl WindowsDragDataObject {
    fn payload(&self, format: *const FORMATETC) -> Option<&[u8]> {
        let format = unsafe { format.as_ref() }?;
        if format.dwAspect != DVASPECT_CONTENT.0 || format.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return None;
        }
        self.formats
            .iter()
            .find(|(cf, _)| *cf == format.cfFormat)
            .map(|(_, bytes)| bytes.as_slice())
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for WindowsDragDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let bytes = self
            .payload(pformatetcin)
            .ok_or_else(|| windows::core::Error::from(DV_E_FORMATETC))?;
        unsafe {
            let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
            let target = GlobalLock(hglobal);
            if target.is_null() {
                let _ = GlobalFree(Some(hglobal));
                return Err(windows::core::Error::from(E_OUTOFMEMORY));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
            let _ = GlobalUnlock(hglobal);
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: hglobal },
                pUnkForRelease: ManuallyDrop::new(None),
            })
        }
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(DV_E_FORMATETC))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if self.payload(pformatetc).is_some() {
            S_OK
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        if let Some(out) = unsafe { pformatetcout.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: windows::core::BOOL,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(windows::core::Error::from(E_NOTIMPL));
        }
        let formats = self
            .formats
            .iter()
            .map(|(format, _)| hglobal_format(*format))
            .collect::<Vec<_>>();
        unsafe { SHCreateStdEnumFmtEtc(&formats) }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: windows::core::Ref<'_, IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(windows::core::Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(windows::core::Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(windows::core::Error::from(OLE_E_ADVISENOTSUPPORTED))
    }
}

#[implement(IDropSource)]
struct WindowsDragSource {
    window: Rc<WindowsWindowInner>,
    use_default_cursors: bool,
}

#[allow(non_snake_case)]
impl IDropSource_Impl for WindowsDragSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows::core::BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if !grfkeystate.contains(MK_LBUTTON) {
            return DRAGDROP_S_DROP;
        }
        self.window.emit_drag_session_moved();
        self.window.run_pending_foreground_tasks();
        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        if self.use_default_cursors {
            DRAGDROP_S_USEDEFAULTCURSORS
        } else {
            // The app draws its own drag feedback, so OLE must not replace the
            // cursor with the drop-effect glyphs.
            S_OK
        }
    }
}

#[implement(IDropTarget)]
struct WindowsDragDropHandler(pub Rc<WindowsWindowInner>);

impl WindowsDragDropHandler {
    fn handle_drag_drop(&self, input: PlatformInput) {
        if let Some(mut func) = self.0.state.callbacks.input.take() {
            func(input);
            self.0.state.callbacks.input.set(Some(func));
        }
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for WindowsDragDropHandler_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows::core::Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            let idata_obj = pdataobj.ok()?;
            let config = FORMATETC {
                cfFormat: CF_HDROP.0,
                ptd: std::ptr::null_mut() as _,
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as _,
            };
            let cursor_position = POINT { x: pt.x, y: pt.y };
            // An app-private drag carries no paths, but the empty list still
            // has to reach the app to restore a suspended in-app drag.
            let has_files = idata_obj.QueryGetData(&config as _) == S_OK;
            if has_files || is_private_drag(idata_obj) {
                *pdweffect = DROPEFFECT_COPY;
                let mut paths = SmallVec::<[PathBuf; 2]>::new();
                if has_files {
                    let Some(mut idata) = idata_obj.GetData(&config as _).log_err() else {
                        return Ok(());
                    };
                    if idata.u.hGlobal.is_invalid() {
                        return Ok(());
                    }
                    let hdrop = HDROP(idata.u.hGlobal.0);
                    with_file_names(hdrop, |file_name| {
                        if let Some(path) = PathBuf::from_str(&file_name).log_err() {
                            paths.push(path);
                        }
                    });
                    ReleaseStgMedium(&mut idata);
                }
                let mut cursor_position = cursor_position;
                ScreenToClient(self.0.hwnd, &mut cursor_position)
                    .ok()
                    .log_err();
                let scale_factor = self.0.state.scale_factor.get();
                let input = PlatformInput::FileDrop(FileDropEvent::Entered {
                    position: logical_point(
                        cursor_position.x as f32,
                        cursor_position.y as f32,
                        scale_factor,
                    ),
                    paths: ExternalPaths(paths),
                });
                self.handle_drag_drop(input);
            } else {
                *pdweffect = DROPEFFECT_NONE;
            }
            self.0
                .drop_target_helper
                .DragEnter(self.0.hwnd, idata_obj, &cursor_position, *pdweffect)
                .log_err();
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let mut cursor_position = POINT { x: pt.x, y: pt.y };
        unsafe {
            *pdweffect = DROPEFFECT_COPY;
            self.0
                .drop_target_helper
                .DragOver(&cursor_position, *pdweffect)
                .log_err();
            ScreenToClient(self.0.hwnd, &mut cursor_position)
                .ok()
                .log_err();
        }
        let scale_factor = self.0.state.scale_factor.get();
        let input = PlatformInput::FileDrop(FileDropEvent::Pending {
            position: logical_point(
                cursor_position.x as f32,
                cursor_position.y as f32,
                scale_factor,
            ),
        });
        self.handle_drag_drop(input);

        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        unsafe {
            self.0.drop_target_helper.DragLeave().log_err();
        }
        let input = PlatformInput::FileDrop(FileDropEvent::Exited);
        self.handle_drag_drop(input);

        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows::core::Ref<IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let idata_obj = pdataobj.ok()?;
        let mut cursor_position = POINT { x: pt.x, y: pt.y };
        unsafe {
            *pdweffect = DROPEFFECT_COPY;
            self.0
                .drop_target_helper
                .Drop(idata_obj, &cursor_position, *pdweffect)
                .log_err();
            ScreenToClient(self.0.hwnd, &mut cursor_position)
                .ok()
                .log_err();
        }
        let scale_factor = self.0.state.scale_factor.get();
        let input = PlatformInput::FileDrop(FileDropEvent::Submit {
            position: logical_point(
                cursor_position.x as f32,
                cursor_position.y as f32,
                scale_factor,
            ),
        });
        self.handle_drag_drop(input);

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ClickState {
    button: Cell<MouseButton>,
    last_click: Cell<Instant>,
    last_position: Cell<Point<DevicePixels>>,
    double_click_spatial_tolerance_width: Cell<i32>,
    double_click_spatial_tolerance_height: Cell<i32>,
    double_click_interval: Cell<Duration>,
    pub(crate) current_count: Cell<usize>,
}

impl ClickState {
    pub fn new() -> Self {
        let double_click_spatial_tolerance_width = unsafe { GetSystemMetrics(SM_CXDOUBLECLK) };
        let double_click_spatial_tolerance_height = unsafe { GetSystemMetrics(SM_CYDOUBLECLK) };
        let double_click_interval = Duration::from_millis(unsafe { GetDoubleClickTime() } as u64);

        ClickState {
            button: Cell::new(MouseButton::Left),
            last_click: Cell::new(Instant::now()),
            last_position: Cell::new(Point::default()),
            double_click_spatial_tolerance_width: Cell::new(double_click_spatial_tolerance_width),
            double_click_spatial_tolerance_height: Cell::new(double_click_spatial_tolerance_height),
            double_click_interval: Cell::new(double_click_interval),
            current_count: Cell::new(0),
        }
    }

    /// update self and return the needed click count
    pub fn update(&self, button: MouseButton, new_position: Point<DevicePixels>) -> usize {
        if self.button.get() == button && self.is_double_click(new_position) {
            self.current_count.update(|it| it + 1);
        } else {
            self.current_count.set(1);
        }
        self.last_click.set(Instant::now());
        self.last_position.set(new_position);
        self.button.set(button);

        self.current_count.get()
    }

    pub fn system_update(&self, wparam: usize) {
        match wparam {
            // SPI_SETDOUBLECLKWIDTH
            29 => self
                .double_click_spatial_tolerance_width
                .set(unsafe { GetSystemMetrics(SM_CXDOUBLECLK) }),
            // SPI_SETDOUBLECLKHEIGHT
            30 => self
                .double_click_spatial_tolerance_height
                .set(unsafe { GetSystemMetrics(SM_CYDOUBLECLK) }),
            // SPI_SETDOUBLECLICKTIME
            32 => self
                .double_click_interval
                .set(Duration::from_millis(unsafe { GetDoubleClickTime() } as u64)),
            _ => {}
        }
    }

    #[inline]
    fn is_double_click(&self, new_position: Point<DevicePixels>) -> bool {
        let diff = self.last_position.get() - new_position;

        self.last_click.get().elapsed() < self.double_click_interval.get()
            && diff.x.0.abs() <= self.double_click_spatial_tolerance_width.get()
            && diff.y.0.abs() <= self.double_click_spatial_tolerance_height.get()
    }
}

#[derive(Copy, Clone)]
struct StyleAndBounds {
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
}

#[repr(C)]
struct WINDOWCOMPOSITIONATTRIBDATA {
    attrib: u32,
    pv_data: *mut std::ffi::c_void,
    cb_data: usize,
}

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

type Color = (u8, u8, u8, u8);

#[derive(Debug, Default, Clone)]
pub(crate) struct WindowBorderOffset {
    pub(crate) width_offset: Cell<i32>,
    pub(crate) height_offset: Cell<i32>,
}

impl WindowBorderOffset {
    pub(crate) fn update(&self, hwnd: HWND) -> anyhow::Result<()> {
        let window_rect = unsafe {
            let mut rect = std::mem::zeroed();
            GetWindowRect(hwnd, &mut rect)?;
            rect
        };
        let client_rect = unsafe {
            let mut rect = std::mem::zeroed();
            GetClientRect(hwnd, &mut rect)?;
            rect
        };
        self.width_offset
            .set((window_rect.right - window_rect.left) - (client_rect.right - client_rect.left));
        self.height_offset
            .set((window_rect.bottom - window_rect.top) - (client_rect.bottom - client_rect.top));
        Ok(())
    }
}

#[derive(Clone)]
struct WindowOpenStatus {
    placement: WINDOWPLACEMENT,
    state: WindowOpenState,
}

#[derive(Clone, Copy)]
enum WindowOpenState {
    Maximized,
    Fullscreen,
    Windowed,
}

const WINDOW_CLASS_NAME: PCWSTR = w!("Zed::Window");

fn register_window_class(icon_handle: HICON) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hIcon: icon_handle,
            lpszClassName: PCWSTR(WINDOW_CLASS_NAME.as_ptr()),
            style: CS_HREDRAW | CS_VREDRAW,
            hInstance: get_module_handle().into(),
            hbrBackground: unsafe { CreateSolidBrush(COLORREF(0x00000000)) },
            ..Default::default()
        };
        unsafe { RegisterClassW(&wc) };
    });
}

unsafe extern "system" fn window_procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let window_params = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        let window_creation_context = window_params.lpCreateParams as *mut WindowCreateContext;
        let window_creation_context = unsafe { &mut *window_creation_context };
        return match WindowsWindowInner::new(window_creation_context, hwnd, window_params) {
            Ok(window_state) => {
                let weak = Box::new(Rc::downgrade(&window_state));
                unsafe { set_window_long(hwnd, GWLP_USERDATA, Box::into_raw(weak) as isize) };
                window_creation_context.inner = Some(Ok(window_state));
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            Err(error) => {
                window_creation_context.inner = Some(Err(error));
                LRESULT(0)
            }
        };
    }

    let ptr = unsafe { get_window_long(hwnd, GWLP_USERDATA) } as *mut Weak<WindowsWindowInner>;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let inner = unsafe { &*ptr };
    let result = if let Some(inner) = inner.upgrade() {
        inner.handle_msg(hwnd, msg, wparam, lparam)
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    };

    if msg == WM_NCDESTROY {
        unsafe { set_window_long(hwnd, GWLP_USERDATA, 0) };
        unsafe { drop(Box::from_raw(ptr)) };
    }

    result
}

pub(crate) fn window_from_hwnd(hwnd: HWND) -> Option<Rc<WindowsWindowInner>> {
    if hwnd.is_invalid() {
        return None;
    }

    let ptr = unsafe { get_window_long(hwnd, GWLP_USERDATA) } as *mut Weak<WindowsWindowInner>;
    if !ptr.is_null() {
        let inner = unsafe { &*ptr };
        inner.upgrade()
    } else {
        None
    }
}

fn get_module_handle() -> HMODULE {
    unsafe {
        let mut h_module = std::mem::zeroed();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::w!("ZedModule"),
            &mut h_module,
        )
        .expect("Unable to get module handle"); // this should never fail

        h_module
    }
}

fn register_drag_drop(window: &Rc<WindowsWindowInner>) -> Result<()> {
    let window_handle = window.hwnd;
    let handler = WindowsDragDropHandler(window.clone());
    // The lifetime of `IDropTarget` is handled by Windows, it won't release until
    // we call `RevokeDragDrop`.
    // So, it's safe to drop it here.
    let drag_drop_handler: IDropTarget = handler.into();
    unsafe {
        RegisterDragDrop(window_handle, &drag_drop_handler)
            .context("unable to register drag-drop event")?;
    }
    Ok(())
}

fn calculate_window_rect(bounds: Bounds<DevicePixels>, border_offset: &WindowBorderOffset) -> RECT {
    // NOTE:
    // The reason we're not using `AdjustWindowRectEx()` here is
    // that the size reported by this function is incorrect.
    // You can test it, and there are similar discussions online.
    // See: https://stackoverflow.com/questions/12423584/how-to-set-exact-client-size-for-overlapped-window-winapi
    //
    // So we manually calculate these values here.
    let mut rect = RECT {
        left: bounds.left().0,
        top: bounds.top().0,
        right: bounds.right().0,
        bottom: bounds.bottom().0,
    };
    let left_offset = border_offset.width_offset.get() / 2;
    let top_offset = border_offset.height_offset.get() / 2;
    let right_offset = border_offset.width_offset.get() - left_offset;
    let bottom_offset = border_offset.height_offset.get() - top_offset;
    rect.left -= left_offset;
    rect.top -= top_offset;
    rect.right += right_offset;
    rect.bottom += bottom_offset;
    rect
}

fn calculate_client_rect(
    rect: RECT,
    border_offset: &WindowBorderOffset,
    scale_factor: f32,
) -> Bounds<Pixels> {
    let left_offset = border_offset.width_offset.get() / 2;
    let top_offset = border_offset.height_offset.get() / 2;
    let right_offset = border_offset.width_offset.get() - left_offset;
    let bottom_offset = border_offset.height_offset.get() - top_offset;
    let left = rect.left + left_offset;
    let top = rect.top + top_offset;
    let right = rect.right - right_offset;
    let bottom = rect.bottom - bottom_offset;
    let physical_size = size(DevicePixels(right - left), DevicePixels(bottom - top));
    Bounds {
        origin: logical_point(left as f32, top as f32, scale_factor),
        size: physical_size.to_pixels(scale_factor),
    }
}

fn retrieve_window_placement(
    hwnd: HWND,
    display: WindowsDisplay,
    initial_bounds: Bounds<Pixels>,
    scale_factor: f32,
    border_offset: &WindowBorderOffset,
    honor_given_bounds: bool,
) -> Result<WINDOWPLACEMENT> {
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe { GetWindowPlacement(hwnd, &mut placement)? };
    // the bounds may be not inside the display
    let bounds = if honor_given_bounds || display.check_given_bounds(initial_bounds) {
        initial_bounds
    } else {
        display.default_bounds()
    };
    let bounds = bounds.to_device_pixels(scale_factor);
    placement.rcNormalPosition = calculate_window_rect(bounds, border_offset);
    Ok(placement)
}

fn dwm_set_window_composition_attribute(hwnd: HWND, backdrop_type: u32) {
    let mut version = unsafe { std::mem::zeroed() };
    let status = unsafe { windows::Wdk::System::SystemServices::RtlGetVersion(&mut version) };

    // DWMWA_SYSTEMBACKDROP_TYPE is available only on version 22621 or later
    // using SetWindowCompositionAttributeType as a fallback
    if !status.is_ok() || version.dwBuildNumber < 22621 {
        return;
    }

    unsafe {
        let result = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop_type as *const _ as *const _,
            std::mem::size_of_val(&backdrop_type) as u32,
        );

        if !result.is_ok() {
            return;
        }
    }
}

fn set_window_composition_attribute(hwnd: HWND, color: Option<Color>, state: u32) {
    let mut version = unsafe { std::mem::zeroed() };
    let status = unsafe { windows::Wdk::System::SystemServices::RtlGetVersion(&mut version) };

    if !status.is_ok() || version.dwBuildNumber < 17763 {
        return;
    }

    unsafe {
        type SetWindowCompositionAttributeType =
            unsafe extern "system" fn(HWND, *mut WINDOWCOMPOSITIONATTRIBDATA) -> BOOL;
        let module_name = PCSTR::from_raw(c"user32.dll".as_ptr() as *const u8);
        if let Some(user32) = GetModuleHandleA(module_name)
            .context("Unable to get user32.dll handle")
            .log_err()
        {
            let func_name = PCSTR::from_raw(c"SetWindowCompositionAttribute".as_ptr() as *const u8);
            let Some(raw_set_window_composition_attribute) = GetProcAddress(user32, func_name)
            else {
                return;
            };
            let set_window_composition_attribute: SetWindowCompositionAttributeType =
                std::mem::transmute(raw_set_window_composition_attribute);
            let mut color = color.unwrap_or_default();
            let is_acrylic = state == 4;
            if is_acrylic && color.3 == 0 {
                color.3 = 1;
            }
            let accent = AccentPolicy {
                accent_state: state,
                accent_flags: if is_acrylic { 0 } else { 2 },
                gradient_color: (color.0 as u32)
                    | ((color.1 as u32) << 8)
                    | ((color.2 as u32) << 16)
                    | ((color.3 as u32) << 24),
                animation_id: 0,
            };
            let mut data = WINDOWCOMPOSITIONATTRIBDATA {
                attrib: 0x13,
                pv_data: &accent as *const _ as *mut _,
                cb_data: std::mem::size_of::<AccentPolicy>(),
            };
            let _ = set_window_composition_attribute(hwnd, &mut data as *mut _ as _);
        }
    }
}

// When the platform title bar is hidden, Windows may think that our application is meant to appear 'fullscreen'
// and will stop the taskbar from appearing on top of our window. Prevent this.
// https://devblogs.microsoft.com/oldnewthing/20250522-00/?p=111211
fn set_non_rude_hwnd(hwnd: HWND, non_rude: bool) {
    if non_rude {
        unsafe { SetPropW(hwnd, w!("NonRudeHWND"), Some(HANDLE(1 as _))) }.log_err();
    } else {
        unsafe { RemovePropW(hwnd, w!("NonRudeHWND")) }.log_err();
    }
}

#[cfg(test)]
mod tests {
    use super::ClickState;
    use gpui::{DevicePixels, MouseButton, point};
    use std::time::Duration;

    #[test]
    fn test_double_click_interval() {
        let state = ClickState::new();
        assert_eq!(
            state.update(MouseButton::Left, point(DevicePixels(0), DevicePixels(0))),
            1
        );
        assert_eq!(
            state.update(MouseButton::Right, point(DevicePixels(0), DevicePixels(0))),
            1
        );
        assert_eq!(
            state.update(MouseButton::Left, point(DevicePixels(0), DevicePixels(0))),
            1
        );
        assert_eq!(
            state.update(MouseButton::Left, point(DevicePixels(0), DevicePixels(0))),
            2
        );
        state
            .last_click
            .update(|it| it - Duration::from_millis(700));
        assert_eq!(
            state.update(MouseButton::Left, point(DevicePixels(0), DevicePixels(0))),
            1
        );
    }

    #[test]
    fn test_double_click_spatial_tolerance() {
        let state = ClickState::new();
        assert_eq!(
            state.update(MouseButton::Left, point(DevicePixels(-3), DevicePixels(0))),
            1
        );
        assert_eq!(
            state.update(MouseButton::Left, point(DevicePixels(0), DevicePixels(3))),
            2
        );
        assert_eq!(
            state.update(MouseButton::Right, point(DevicePixels(3), DevicePixels(2))),
            1
        );
        assert_eq!(
            state.update(MouseButton::Right, point(DevicePixels(10), DevicePixels(0))),
            1
        );
    }
}
