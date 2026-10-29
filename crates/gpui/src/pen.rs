use crate::{
    HitboxId, InputEvent, KeyEvent, Modifiers, MouseEvent, Pixels, PlatformInput, Point, TouchId,
    seal::Sealed,
};
use collections::FxHashSet;
use smallvec::SmallVec;
use std::time::Duration;

/// Where pen input comes from, on platforms that offer more than one source.
///
/// Only Windows offers a choice; other platforms ignore it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PenSource {
    /// Wintab when a tablet driver provides it, Windows Ink otherwise.
    #[default]
    Auto,
    /// Windows Ink: available for every digitizer, but drivers may disable it.
    WindowsInk,
    /// Wintab: full driver resolution, but only with a tablet driver installed. Falls back
    /// to Windows Ink when the driver does not provide it.
    Wintab,
}

/// Which end of the pen produced the event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PenTip {
    /// The writing tip.
    #[default]
    Tip,
    /// The eraser end.
    Eraser,
}

/// The phase of a [`PenEvent`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PenPhase {
    /// The pen came into range of the digitizer without touching it.
    #[default]
    Entered,
    /// The pen touched the digitizer.
    Down,
    /// The pen moved, while hovering or in contact.
    Moved,
    /// The pen lifted off the digitizer.
    Up,
    /// The pen left the range of the digitizer.
    Exited,
    /// The platform cancelled the stroke.
    Cancelled,
    /// Final values for samples that were sent earlier with provisional axes.
    /// May arrive after [`PenPhase::Up`].
    Revised,
}

bitflags::bitflags! {
    /// A set of sample axes.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct PenAxes: u8 {
        /// [`PenSample::position`].
        const POSITION = 1 << 0;
        /// [`PenSample::pressure`].
        const PRESSURE = 1 << 1;
        /// [`PenSample::tilt`].
        const TILT = 1 << 2;
        /// [`PenSample::rotation`].
        const ROTATION = 1 << 3;
    }
}

/// Identifies a sample with provisional axes, so that a later
/// [`PenPhase::Revised`] sample can name the sample it replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PenSampleId(pub u64);

/// Identifies one physical pen across proximity sessions, when the source
/// can tell pens apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PenDeviceId(pub u64);

/// One digitizer sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PenSample {
    /// The position in window coordinates.
    pub position: Point<Pixels>,
    /// Contact pressure, normalized to `0.0..=1.0`. Zero while hovering.
    pub pressure: f32,
    /// Tilt of the pen from the digitizer's normal, in degrees along the
    /// window's x and y axes. `None` when the source has no tilt axis.
    pub tilt: Option<Point<f32>>,
    /// Rotation of the pen around its own axis, in degrees, increasing clockwise
    /// as seen looking down at the digitizer. The zero angle depends on the
    /// source. `None` when the source has no rotation axis.
    pub rotation: Option<f32>,
    /// Airbrush finger wheel. `None` when the source has no such axis.
    pub wheel: Option<f32>,
    /// Hover height above the digitizer, normalized to `0.0..=1.0`.
    /// `None` when the source has no such axis.
    pub distance: Option<f32>,
    /// When the digitizer took this sample, on the platform's monotonic input
    /// clock. Only differences between timestamps are meaningful.
    pub timestamp: Duration,
    /// Axes reported before their value was final. A later
    /// [`PenPhase::Revised`] event replaces them. Empty on sources that only
    /// report final values.
    pub provisional: PenAxes,
    /// `Some` when `provisional` is not empty, and on [`PenPhase::Revised`]
    /// samples, where it names the sample being replaced.
    pub id: Option<PenSampleId>,
}

/// A pen event from the platform.
///
/// A stroke is claimed by calling [`Window::prevent_default`](crate::Window::prevent_default)
/// while handling its [`PenPhase::Down`] event. A claimed stroke delivers every later
/// event, including [`PenPhase::Revised`] events after [`PenPhase::Up`], to the element
/// that claimed it, and the platform does not turn the stroke into touch or mouse
/// input. An unclaimed stroke is handed to the platform's default touch or mouse path
/// and receives no further pen events. Hover events are hit-tested one by one, and the
/// platform turns each unclaimed one into mouse movement.
#[derive(Clone, Debug, Default)]
pub struct PenEvent {
    /// Identifies the stroke, from [`PenPhase::Down`] until its last event.
    pub id: TouchId,
    /// Stays the same while the pen is in proximity. `None` when the source
    /// cannot tell pens apart.
    pub device_id: Option<PenDeviceId>,
    /// The phase of the event.
    pub phase: PenPhase,
    /// Which end of the pen produced the event.
    pub tip: PenTip,
    /// Whether the barrel button is held.
    pub barrel: bool,
    /// Whether the pen is touching the digitizer.
    pub in_contact: bool,
    /// The modifiers held when the event occurred.
    pub modifiers: Modifiers,
    /// Samples since the previous event, oldest first. The last one is
    /// current. For [`PenPhase::Revised`], only the samples being replaced.
    pub samples: SmallVec<[PenSample; 8]>,
    /// Samples the platform predicts the pen will reach next, oldest first.
    /// Empty when the platform offers no prediction. For display only; the
    /// next event replaces them. Never provisional and never carry an id.
    pub predicted: SmallVec<[PenSample; 2]>,
}

impl PenEvent {
    /// The position of the latest sample. For [`PenPhase::Revised`] this is
    /// not the current position of the pen.
    pub fn position(&self) -> Point<Pixels> {
        self.samples
            .last()
            .map(|sample| sample.position)
            .unwrap_or_default()
    }
}

impl Sealed for PenEvent {}
impl InputEvent for PenEvent {
    fn to_platform_input(self) -> PlatformInput {
        PlatformInput::Pen(self)
    }
}
impl MouseEvent for PenEvent {}

/// The phase of a held [`PenGesture`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenGesturePhase {
    /// The gesture started.
    Began,
    /// The gesture is still held.
    Changed,
    /// The gesture finished.
    Ended,
    /// The platform cancelled the gesture.
    Cancelled,
}

/// A gesture performed on the pen body rather than on the digitizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenGesture {
    /// Two quick taps on the pen barrel.
    DoubleTap,
    /// Gripping the pen barrel harder, held over time.
    Squeeze(PenGesturePhase),
}

/// A pen gesture event, delivered along the focus path like key events.
#[derive(Clone, Debug, PartialEq)]
pub struct PenGestureEvent {
    /// The gesture performed.
    pub gesture: PenGesture,
    /// The pen that performed it, when the source can tell pens apart.
    pub device_id: Option<PenDeviceId>,
    /// When the gesture occurred, on the platform's monotonic input clock.
    pub timestamp: Duration,
    /// The pen's pose, when it was hovering during the gesture.
    pub hover: Option<PenSample>,
}

impl Sealed for PenGestureEvent {}
impl InputEvent for PenGestureEvent {
    fn to_platform_input(self) -> PlatformInput {
        PlatformInput::PenGesture(self)
    }
}
impl KeyEvent for PenGestureEvent {}

pub(crate) enum PenRoute {
    HitTest,
    Claimed(Option<HitboxId>),
    YieldToScroll {
        hitbox: Option<HitboxId>,
        start: Point<Pixels>,
    },
    Scroll(TouchId),
    Consumed,
    Declined,
}

enum PenStroke {
    Claimed {
        touch: TouchId,
        hitbox: Option<HitboxId>,
        contact: bool,
        provisional: FxHashSet<PenSampleId>,
        scroll_start: Option<Point<Pixels>>,
    },
    Scrolling {
        touch: TouchId,
        scroll: TouchId,
    },
    Declined {
        touch: TouchId,
    },
}

#[derive(Default)]
pub(crate) struct PenStrokes {
    stroke: Option<PenStroke>,
    claiming_hitbox: Option<HitboxId>,
    yields_to_scroll: bool,
    next_scroll_id: u64,
}

impl PenStrokes {
    pub(crate) fn route(&mut self, event: &PenEvent, scroll_slop: Pixels) -> PenRoute {
        match event.phase {
            PenPhase::Down => {
                self.stroke = None;
                self.claiming_hitbox = None;
                self.yields_to_scroll = false;
                PenRoute::HitTest
            }
            PenPhase::Entered | PenPhase::Exited => PenRoute::HitTest,
            PenPhase::Moved if !event.in_contact => PenRoute::HitTest,
            PenPhase::Moved | PenPhase::Up | PenPhase::Cancelled | PenPhase::Revised => {
                match &self.stroke {
                    Some(PenStroke::Claimed {
                        touch,
                        hitbox,
                        scroll_start: Some(start),
                        ..
                    }) if *touch == event.id
                        && event.phase == PenPhase::Moved
                        && (event.position() - *start).magnitude() > f64::from(scroll_slop) =>
                    {
                        PenRoute::YieldToScroll {
                            hitbox: *hitbox,
                            start: *start,
                        }
                    }
                    Some(PenStroke::Claimed { touch, hitbox, .. }) if *touch == event.id => {
                        PenRoute::Claimed(*hitbox)
                    }
                    Some(PenStroke::Scrolling { touch, scroll }) if *touch == event.id => {
                        if event.phase == PenPhase::Revised {
                            PenRoute::Consumed
                        } else {
                            PenRoute::Scroll(*scroll)
                        }
                    }
                    Some(PenStroke::Declined { touch }) if *touch == event.id => PenRoute::Declined,
                    _ if event.phase == PenPhase::Revised => PenRoute::Declined,
                    _ => PenRoute::HitTest,
                }
            }
        }
    }

    pub(crate) fn offer_hitbox(&mut self, hitbox: HitboxId) {
        self.claiming_hitbox.get_or_insert(hitbox);
    }

    pub(crate) fn yield_to_scroll(&mut self) {
        self.yields_to_scroll = true;
    }

    pub(crate) fn yields_to_scroll(&self) -> bool {
        self.yields_to_scroll
    }

    pub(crate) fn start_scroll(&mut self, touch: TouchId) -> TouchId {
        self.next_scroll_id += 1;
        let scroll = TouchId(u64::MAX - self.next_scroll_id);
        self.stroke = Some(PenStroke::Scrolling { touch, scroll });
        scroll
    }

    pub(crate) fn rebind(&mut self, previous: HitboxId, current: HitboxId) {
        if let Some(PenStroke::Claimed {
            hitbox: Some(hitbox),
            ..
        }) = &mut self.stroke
            && *hitbox == previous
        {
            *hitbox = current;
        }
    }

    pub(crate) fn finish(&mut self, event: &PenEvent, claimed: bool, over_scroll_region: bool) {
        if event.phase == PenPhase::Down {
            self.stroke = Some(if claimed {
                PenStroke::Claimed {
                    touch: event.id,
                    hitbox: self.claiming_hitbox.take(),
                    contact: true,
                    provisional: FxHashSet::default(),
                    scroll_start: (self.yields_to_scroll && over_scroll_region)
                        .then(|| event.position()),
                }
            } else {
                PenStroke::Declined { touch: event.id }
            });
            self.claiming_hitbox = None;
        }

        let release = match &mut self.stroke {
            Some(PenStroke::Claimed {
                touch,
                contact,
                provisional,
                ..
            }) if *touch == event.id => {
                for sample in &event.samples {
                    if let Some(id) = sample.id {
                        if sample.provisional.is_empty() {
                            provisional.remove(&id);
                        } else {
                            provisional.insert(id);
                        }
                    }
                }
                if event.phase == PenPhase::Up {
                    *contact = false;
                }
                event.phase == PenPhase::Cancelled || (!*contact && provisional.is_empty())
            }
            Some(PenStroke::Scrolling { touch, .. } | PenStroke::Declined { touch })
                if *touch == event.id =>
            {
                matches!(event.phase, PenPhase::Up | PenPhase::Cancelled)
            }
            _ => false,
        };
        if release {
            self.stroke = None;
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Context, DispatchEventResult, FocusHandle, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, TestAppContext,
        Window, WindowHandle, div, point, px,
    };
    use std::{cell::RefCell, rc::Rc};

    type Log = Rc<RefCell<Vec<(&'static str, PenPhase)>>>;

    struct PenView {
        log: Log,
        claim_down: bool,
        claim_hover: bool,
        focus_handle: FocusHandle,
    }

    impl Render for PenView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let canvas_log = self.log.clone();
            let other_log = self.log.clone();
            let click_log = self.log.clone();
            let gesture_log = self.log.clone();
            let claim_down = self.claim_down;
            let claim_hover = self.claim_hover;
            div()
                .size_full()
                .track_focus(&self.focus_handle)
                .on_pen_gesture(move |_, _, _| {
                    gesture_log
                        .borrow_mut()
                        .push(("gesture", PenPhase::Entered));
                })
                .child(
                    div()
                        .id("canvas")
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(100.))
                        .h(px(100.))
                        .on_pen(move |event, window, _| {
                            canvas_log.borrow_mut().push(("canvas", event.phase));
                            let hovering = !event.in_contact
                                && matches!(event.phase, PenPhase::Entered | PenPhase::Moved);
                            if (claim_down && event.phase == PenPhase::Down)
                                || (claim_hover && hovering)
                            {
                                window.prevent_default();
                            }
                        }),
                )
                .child(
                    div()
                        .id("other")
                        .absolute()
                        .left(px(200.))
                        .top(px(0.))
                        .w(px(100.))
                        .h(px(100.))
                        .on_pen(move |event, _, _| {
                            other_log.borrow_mut().push(("other", event.phase));
                        }),
                )
                .child(
                    div()
                        .id("button")
                        .absolute()
                        .left(px(400.))
                        .top(px(0.))
                        .w(px(100.))
                        .h(px(100.))
                        .on_click(move |_, _, _| {
                            click_log.borrow_mut().push(("click", PenPhase::Up));
                        }),
                )
        }
    }

    fn open(
        cx: &mut TestAppContext,
        claim_down: bool,
        claim_hover: bool,
    ) -> (WindowHandle<PenView>, Log) {
        let log = Log::default();
        let window = cx.add_window({
            let log = log.clone();
            move |_, cx| PenView {
                log,
                claim_down,
                claim_hover,
                focus_handle: cx.focus_handle(),
            }
        });
        (window, log)
    }

    fn sample(x: f32, id: Option<u64>, provisional: PenAxes) -> PenSample {
        PenSample {
            position: point(px(x), px(50.)),
            pressure: 0.5,
            provisional,
            id: id.map(PenSampleId),
            ..Default::default()
        }
    }

    fn send(
        window: WindowHandle<PenView>,
        cx: &mut TestAppContext,
        phase: PenPhase,
        in_contact: bool,
        samples: &[PenSample],
    ) -> DispatchEventResult {
        window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PenEvent {
                        id: TouchId(1),
                        phase,
                        in_contact,
                        samples: samples.iter().copied().collect(),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                )
            })
            .unwrap()
    }

    #[crate::test]
    fn claimed_stroke_follows_the_claiming_element(cx: &mut TestAppContext) {
        let (window, log) = open(cx, true, false);
        let none = PenAxes::empty();

        assert!(
            send(window, cx, PenPhase::Down, true, &[sample(50., None, none)]).default_prevented
        );
        assert!(
            send(
                window,
                cx,
                PenPhase::Moved,
                true,
                &[sample(250., None, none)]
            )
            .default_prevented
        );
        assert!(
            send(window, cx, PenPhase::Up, false, &[sample(250., None, none)]).default_prevented
        );

        assert_eq!(
            log.borrow().as_slice(),
            [
                ("canvas", PenPhase::Down),
                ("canvas", PenPhase::Moved),
                ("canvas", PenPhase::Up),
            ]
        );
    }

    #[crate::test]
    fn claimed_stroke_survives_a_new_frame(cx: &mut TestAppContext) {
        let (window, log) = open(cx, true, false);
        let none = PenAxes::empty();

        send(window, cx, PenPhase::Down, true, &[sample(50., None, none)]);
        window.update(cx, |_, window, _| window.refresh()).unwrap();
        cx.run_until_parked();
        assert!(
            send(
                window,
                cx,
                PenPhase::Moved,
                true,
                &[sample(150., None, none)]
            )
            .default_prevented
        );

        assert_eq!(
            *log.borrow(),
            [("canvas", PenPhase::Down), ("canvas", PenPhase::Moved)]
        );
    }

    #[crate::test]
    fn pen_clicks_an_element_released_inside_it(cx: &mut TestAppContext) {
        let (window, log) = open(cx, false, false);
        let none = PenAxes::empty();

        assert!(
            send(
                window,
                cx,
                PenPhase::Down,
                true,
                &[sample(450., None, none)]
            )
            .default_prevented
        );
        window.update(cx, |_, window, _| window.refresh()).unwrap();
        cx.run_until_parked();
        send(
            window,
            cx,
            PenPhase::Moved,
            true,
            &[sample(470., None, none)],
        );
        send(window, cx, PenPhase::Up, false, &[sample(470., None, none)]);
        assert_eq!(*log.borrow(), [("click", PenPhase::Up)]);

        log.borrow_mut().clear();
        send(
            window,
            cx,
            PenPhase::Down,
            true,
            &[sample(450., None, none)],
        );
        send(
            window,
            cx,
            PenPhase::Moved,
            true,
            &[sample(550., None, none)],
        );
        send(window, cx, PenPhase::Up, false, &[sample(550., None, none)]);
        assert!(log.borrow().is_empty());
    }

    #[crate::test]
    fn unclaimed_stroke_falls_back_for_its_whole_length(cx: &mut TestAppContext) {
        let (window, log) = open(cx, false, false);
        let none = PenAxes::empty();

        assert!(
            !send(window, cx, PenPhase::Down, true, &[sample(50., None, none)]).default_prevented
        );
        assert!(
            !send(
                window,
                cx,
                PenPhase::Moved,
                true,
                &[sample(60., None, none)]
            )
            .default_prevented
        );
        assert!(
            !send(window, cx, PenPhase::Up, false, &[sample(60., None, none)]).default_prevented
        );

        assert_eq!(log.borrow().as_slice(), [("canvas", PenPhase::Down)]);
    }

    #[crate::test]
    fn revisions_after_up_reach_the_claiming_element(cx: &mut TestAppContext) {
        let (window, log) = open(cx, true, false);
        let pressure = PenAxes::PRESSURE;
        let none = PenAxes::empty();

        send(
            window,
            cx,
            PenPhase::Down,
            true,
            &[sample(50., Some(7), pressure)],
        );
        send(window, cx, PenPhase::Up, false, &[sample(50., None, none)]);
        assert!(
            send(
                window,
                cx,
                PenPhase::Revised,
                false,
                &[sample(50., Some(7), none)]
            )
            .default_prevented
        );
        assert!(
            !send(
                window,
                cx,
                PenPhase::Revised,
                false,
                &[sample(50., Some(7), none)]
            )
            .default_prevented
        );

        assert_eq!(
            log.borrow().as_slice(),
            [
                ("canvas", PenPhase::Down),
                ("canvas", PenPhase::Up),
                ("canvas", PenPhase::Revised),
            ]
        );
    }

    #[crate::test]
    fn hover_is_judged_event_by_event(cx: &mut TestAppContext) {
        let (window, _) = open(cx, false, true);
        let none = PenAxes::empty();

        assert!(
            send(
                window,
                cx,
                PenPhase::Moved,
                false,
                &[sample(50., None, none)]
            )
            .default_prevented
        );
        assert!(
            !send(
                window,
                cx,
                PenPhase::Moved,
                false,
                &[sample(250., None, none)]
            )
            .default_prevented
        );
    }

    #[crate::test]
    fn pen_gestures_follow_focus(cx: &mut TestAppContext) {
        let (window, log) = open(cx, false, false);
        window
            .update(cx, |view, window, cx| window.focus(&view.focus_handle, cx))
            .unwrap();
        window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PenGestureEvent {
                        gesture: PenGesture::DoubleTap,
                        device_id: None,
                        timestamp: Duration::ZERO,
                        hover: None,
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .unwrap();

        assert_eq!(log.borrow().as_slice(), [("gesture", PenPhase::Entered)]);
    }

    struct ScrollingView {
        log: Log,
        scroll: crate::ScrollHandle,
    }

    impl Render for ScrollingView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let click_log = self.log.clone();
            let pen_log = self.log.clone();
            div().size_full().child(
                div()
                    .id("list")
                    .w(px(100.))
                    .h(px(100.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().id("row").h(px(40.)).on_click(move |_, _, _| {
                        click_log.borrow_mut().push(("click", PenPhase::Up));
                    }))
                    .child(
                        div()
                            .id("swatch")
                            .h(px(40.))
                            .on_pen(move |event, window, _| {
                                pen_log.borrow_mut().push(("swatch", event.phase));
                                if event.phase == PenPhase::Down {
                                    window.claim_pen_unless_scrolled();
                                }
                            }),
                    )
                    .child(div().h(px(400.))),
            )
        }
    }

    fn open_scrolling(
        cx: &mut TestAppContext,
    ) -> (WindowHandle<ScrollingView>, Log, crate::ScrollHandle) {
        let log = Log::default();
        let scroll = crate::ScrollHandle::new();
        let window = cx.add_window({
            let log = log.clone();
            let scroll = scroll.clone();
            move |_, _| ScrollingView { log, scroll }
        });
        (window, log, scroll)
    }

    fn send_at(
        window: WindowHandle<ScrollingView>,
        cx: &mut TestAppContext,
        phase: PenPhase,
        in_contact: bool,
        y: f32,
    ) -> DispatchEventResult {
        window
            .update(cx, |_, window, cx| {
                window.dispatch_event(
                    PenEvent {
                        id: TouchId(1),
                        phase,
                        in_contact,
                        samples: std::iter::once(PenSample {
                            position: point(px(50.), px(y)),
                            pressure: 0.5,
                            ..Default::default()
                        })
                        .collect(),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                )
            })
            .unwrap()
    }

    #[crate::test]
    fn click_in_a_scroll_region_yields_to_scrolling(cx: &mut TestAppContext) {
        let (window, log, scroll) = open_scrolling(cx);

        assert!(send_at(window, cx, PenPhase::Down, true, 20.).default_prevented);
        assert!(send_at(window, cx, PenPhase::Moved, true, 16.).default_prevented);
        assert_eq!(scroll.offset().y, px(0.));
        assert!(send_at(window, cx, PenPhase::Moved, true, 0.).default_prevented);
        assert!(send_at(window, cx, PenPhase::Up, false, 0.).default_prevented);
        cx.run_until_parked();

        assert!(log.borrow().is_empty());
        assert!(scroll.offset().y < px(0.));
    }

    #[crate::test]
    fn click_in_a_scroll_region_still_clicks_without_moving(cx: &mut TestAppContext) {
        let (window, log, scroll) = open_scrolling(cx);

        send_at(window, cx, PenPhase::Down, true, 20.);
        send_at(window, cx, PenPhase::Moved, true, 16.);
        send_at(window, cx, PenPhase::Up, false, 16.);

        assert_eq!(*log.borrow(), [("click", PenPhase::Up)]);
        assert_eq!(scroll.offset().y, px(0.));
    }

    #[crate::test]
    fn pen_listener_can_yield_to_scrolling(cx: &mut TestAppContext) {
        let (window, log, scroll) = open_scrolling(cx);

        send_at(window, cx, PenPhase::Down, true, 60.);
        send_at(window, cx, PenPhase::Moved, true, 40.);
        send_at(window, cx, PenPhase::Moved, true, 30.);
        send_at(window, cx, PenPhase::Up, false, 30.);
        cx.run_until_parked();

        assert_eq!(
            *log.borrow(),
            [("swatch", PenPhase::Down), ("swatch", PenPhase::Cancelled)]
        );
        assert!(scroll.offset().y < px(0.));
    }
}
