//! Apple Pencil test bench for GPUI pen input.
//!
//! The canvas on the left claims pen strokes and draws them. The panel on the right never
//! handles pen events, so pen input over it falls back to touch and mouse input.

#[cfg(target_os = "ios")]
mod app {
    use gpui::{
        App, Bounds, Context, FocusHandle, Hsla, MouseButton, PathBuilder, PenEvent, PenGesture,
        PenGestureEvent, PenPhase, PenSample, PenSampleId, PenTip, Pixels, Point, SharedString,
        TouchId, Window, WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
    };
    use std::collections::{HashMap, VecDeque};
    use std::time::Duration;

    #[derive(Clone, Copy)]
    struct StrokePoint {
        sample: PenSample,
        revised: bool,
    }

    #[derive(Clone)]
    struct Stroke {
        points: Vec<StrokePoint>,
        eraser: bool,
        cancelled: bool,
    }

    #[derive(Default)]
    struct MouseCounts {
        down: usize,
        moved: usize,
        up: usize,
        scroll: usize,
    }

    #[derive(Default)]
    struct Stats {
        events: usize,
        samples: usize,
        max_samples_per_event: usize,
        predicted: usize,
        provisional: usize,
        revised: usize,
        unmatched_revisions: usize,
        declined: usize,
        cancelled: usize,
        last_rate: Option<f32>,
        max_pressure: f32,
    }

    struct PenExample {
        focus_handle: FocusHandle,
        claim: bool,
        strokes: Vec<Stroke>,
        current: Option<(TouchId, usize)>,
        pending: HashMap<PenSampleId, (usize, usize)>,
        predicted: Vec<PenSample>,
        hover: Option<PenSample>,
        pose: Option<PenSample>,
        latest: Option<PenEvent>,
        stats: Stats,
        canvas_mouse: MouseCounts,
        panel_mouse: MouseCounts,
        gesture: SharedString,
        log: VecDeque<SharedString>,
    }

    impl PenExample {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let focus_handle = cx.focus_handle();
            focus_handle.focus(window, cx);
            Self {
                focus_handle,
                claim: true,
                strokes: Vec::new(),
                current: None,
                pending: HashMap::new(),
                predicted: Vec::new(),
                hover: None,
                pose: None,
                latest: None,
                stats: Stats::default(),
                canvas_mouse: MouseCounts::default(),
                panel_mouse: MouseCounts::default(),
                gesture: "none".into(),
                log: VecDeque::new(),
            }
        }

        fn push_log(&mut self, line: String) {
            if self.log.len() == 14 {
                self.log.pop_front();
            }
            self.log.push_back(line.into());
        }

        fn handle_pen(&mut self, event: &PenEvent, window: &mut Window, cx: &mut Context<Self>) {
            self.stats.events += 1;
            self.stats.samples += event.samples.len();
            self.stats.max_samples_per_event =
                self.stats.max_samples_per_event.max(event.samples.len());
            self.stats.predicted += event.predicted.len();
            if !matches!(event.phase, PenPhase::Moved) || !event.in_contact {
                let provisional = event
                    .samples
                    .iter()
                    .filter(|sample| !sample.provisional.is_empty())
                    .count();
                self.push_log(format!(
                    "{:?} #{} {:?} samples={} predicted={} provisional={}",
                    event.phase,
                    event.id.0,
                    event.tip,
                    event.samples.len(),
                    event.predicted.len(),
                    provisional
                ));
            }

            match event.phase {
                PenPhase::Entered | PenPhase::Moved if !event.in_contact => {
                    self.hover = event.samples.last().copied();
                    if self.claim {
                        window.prevent_default();
                    }
                }
                PenPhase::Exited => {
                    self.hover = None;
                    if self.claim {
                        window.prevent_default();
                    }
                }
                PenPhase::Down => {
                    self.hover = None;
                    if !self.claim {
                        self.stats.declined += 1;
                        cx.notify();
                        return;
                    }
                    window.prevent_default();
                    self.strokes.push(Stroke {
                        points: Vec::new(),
                        eraser: event.tip == PenTip::Eraser,
                        cancelled: false,
                    });
                    self.current = Some((event.id, self.strokes.len() - 1));
                    self.append_samples(event);
                }
                PenPhase::Moved | PenPhase::Up => {
                    self.append_samples(event);
                    if event.phase == PenPhase::Up {
                        self.current = None;
                        self.predicted.clear();
                    }
                }
                PenPhase::Cancelled => {
                    self.stats.cancelled += 1;
                    if let Some((_, stroke)) = self.current.take() {
                        self.strokes[stroke].cancelled = true;
                    }
                    self.predicted.clear();
                }
                PenPhase::Revised => self.apply_revisions(event),
                PenPhase::Entered => {}
            }
            match event.phase {
                PenPhase::Up | PenPhase::Exited | PenPhase::Cancelled => self.pose = None,
                PenPhase::Revised => {}
                _ => self.pose = event.samples.last().copied(),
            }
            self.latest = Some(event.clone());
            cx.notify();
        }

        fn append_samples(&mut self, event: &PenEvent) {
            let Some((id, stroke)) = self.current else {
                return;
            };
            if id != event.id {
                return;
            }
            for sample in &event.samples {
                self.stats.max_pressure = self.stats.max_pressure.max(sample.pressure);
                if let Some(sample_id) = sample.id
                    && !sample.provisional.is_empty()
                {
                    self.stats.provisional += 1;
                    self.pending
                        .insert(sample_id, (stroke, self.strokes[stroke].points.len()));
                }
                self.strokes[stroke].points.push(StrokePoint {
                    sample: *sample,
                    revised: false,
                });
            }
            self.predicted = event.predicted.to_vec();
            if event.samples.len() > 1 {
                let first = event.samples[0].timestamp;
                let last = event.samples[event.samples.len() - 1].timestamp;
                let span = last.saturating_sub(first);
                if span > Duration::ZERO {
                    self.stats.last_rate =
                        Some((event.samples.len() - 1) as f32 / span.as_secs_f32());
                }
            }
        }

        fn apply_revisions(&mut self, event: &PenEvent) {
            for sample in &event.samples {
                let Some((stroke, index)) = sample.id.and_then(|id| self.pending.get(&id)).copied()
                else {
                    self.stats.unmatched_revisions += 1;
                    continue;
                };
                let point = &mut self.strokes[stroke].points[index];
                point.sample = *sample;
                point.revised = true;
                if sample.provisional.is_empty() {
                    self.pending.remove(&sample.id.unwrap());
                }
                self.stats.revised += 1;
            }
        }

        fn handle_gesture(&mut self, event: &PenGestureEvent, cx: &mut Context<Self>) {
            self.gesture = match event.gesture {
                PenGesture::DoubleTap => {
                    self.claim = !self.claim;
                    "DoubleTap (claim toggled)".into()
                }
                PenGesture::Squeeze(phase) => format!("Squeeze {phase:?}").into(),
            };
            if let Some(hover) = event.hover {
                self.gesture = format!(
                    "{} at ({:.0}, {:.0})",
                    self.gesture,
                    f32::from(hover.position.x),
                    f32::from(hover.position.y)
                )
                .into();
            }
            self.push_log(format!("gesture {:?}", event.gesture));
            cx.notify();
        }

        fn button(
            &self,
            id: &'static str,
            label: String,
            action: fn(&mut Self),
            cx: &mut Context<Self>,
        ) -> impl IntoElement {
            div()
                .id(id)
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(0x246bfd))
                .on_click(cx.listener(move |this, _, _, cx| {
                    action(this);
                    cx.notify();
                }))
                .child(label)
        }

        fn clear(&mut self) {
            self.strokes.clear();
            self.current = None;
            self.pending.clear();
            self.predicted.clear();
            self.stats = Stats::default();
            self.canvas_mouse = MouseCounts::default();
            self.panel_mouse = MouseCounts::default();
            self.log.clear();
        }

        fn readout(&self) -> Vec<String> {
            let stats = &self.stats;
            let mut lines = vec![
                format!("claim: {}", if self.claim { "on" } else { "off" }),
                format!("gesture: {}", self.gesture),
                format!(
                    "events {} / samples {} / max per event {}",
                    stats.events, stats.samples, stats.max_samples_per_event
                ),
                format!("predicted samples {}", stats.predicted),
                format!(
                    "provisional {} / revised {} / still pending {} / unmatched {}",
                    stats.provisional,
                    stats.revised,
                    self.pending.len(),
                    stats.unmatched_revisions
                ),
                format!(
                    "declined strokes {} / cancelled {}",
                    stats.declined, stats.cancelled
                ),
                format!(
                    "sample rate {} / max pressure {:.2}",
                    stats
                        .last_rate
                        .map_or("-".to_string(), |rate| format!("{rate:.0} Hz")),
                    stats.max_pressure
                ),
                format!(
                    "canvas mouse down {} move {} up {} / scroll {}",
                    self.canvas_mouse.down,
                    self.canvas_mouse.moved,
                    self.canvas_mouse.up,
                    self.canvas_mouse.scroll
                ),
                format!(
                    "panel mouse down {} move {} up {} / scroll {}",
                    self.panel_mouse.down,
                    self.panel_mouse.moved,
                    self.panel_mouse.up,
                    self.panel_mouse.scroll
                ),
            ];
            let sample = self
                .latest
                .as_ref()
                .and_then(|event| event.samples.last().copied())
                .or(self.hover);
            if let Some(sample) = sample {
                lines.push(format!(
                    "position ({:.1}, {:.1}) pressure {:.3}",
                    f32::from(sample.position.x),
                    f32::from(sample.position.y),
                    sample.pressure
                ));
                lines.push(format!(
                    "tilt {} / rotation {} / distance {}",
                    sample.tilt.map_or("-".to_string(), |tilt| format!(
                        "({:.1}, {:.1})",
                        tilt.x, tilt.y
                    )),
                    sample
                        .rotation
                        .map_or("-".to_string(), |rotation| format!("{rotation:.1}")),
                    sample
                        .distance
                        .map_or("-".to_string(), |distance| format!("{distance:.2}")),
                ));
            }
            if let Some(event) = &self.latest {
                lines.push(format!(
                    "device {:?} / barrel {} / in contact {}",
                    event.device_id.map(|id| id.0),
                    event.barrel,
                    event.in_contact
                ));
            }
            lines
        }
    }

    fn paint_segment(
        from: Point<Pixels>,
        to: Point<Pixels>,
        width: f32,
        color: Hsla,
        window: &mut Window,
    ) {
        let mut builder = PathBuilder::stroke(px(width));
        builder.move_to(from);
        builder.line_to(to);
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    fn paint_dot(center: Point<Pixels>, radius: f32, color: Hsla, window: &mut Window) {
        let bounds = Bounds::new(
            point(center.x - px(radius), center.y - px(radius)),
            size(px(radius * 2.), px(radius * 2.)),
        );
        window.paint_quad(fill(bounds, color).corner_radii(px(radius)));
    }

    fn stroke_width(pressure: f32) -> f32 {
        1.0 + pressure * 14.0
    }

    fn paint_strokes(
        strokes: Vec<Stroke>,
        predicted: Vec<PenSample>,
        hover: Option<PenSample>,
        pose: Option<PenSample>,
        window: &mut Window,
    ) {
        for stroke in &strokes {
            let base: Hsla = if stroke.cancelled {
                rgb(0x6b7280).into()
            } else if stroke.eraser {
                rgb(0xf87171).into()
            } else {
                rgb(0xf5f7ff).into()
            };
            for pair in stroke.points.windows(2) {
                let color = if !pair[1].sample.provisional.is_empty() {
                    rgb(0xfbbf24).into()
                } else if pair[1].revised {
                    rgb(0x60a5fa).into()
                } else {
                    base
                };
                let width = stroke_width((pair[0].sample.pressure + pair[1].sample.pressure) / 2.);
                paint_segment(
                    pair[0].sample.position,
                    pair[1].sample.position,
                    width,
                    color,
                    window,
                );
            }
            if let [point] = stroke.points.as_slice() {
                paint_dot(
                    point.sample.position,
                    stroke_width(point.sample.pressure) / 2.,
                    base,
                    window,
                );
            }
            if let Some(last) = stroke.points.last() {
                let mut from = last.sample.position;
                if std::ptr::eq(stroke, strokes.last().unwrap()) {
                    for sample in &predicted {
                        paint_segment(from, sample.position, 3., rgb(0x34d399).into(), window);
                        from = sample.position;
                    }
                }
            }
        }
        if let Some(hover) = hover {
            let radius = 4. + hover.distance.unwrap_or(0.) * 30.;
            paint_dot(
                hover.position,
                radius,
                Hsla::from(rgb(0xa78bfa)).opacity(0.5),
                window,
            );
            if let Some(tilt) = hover.tilt {
                let tip = point(hover.position.x + px(tilt.x), hover.position.y + px(tilt.y));
                paint_segment(hover.position, tip, 2., rgb(0xa78bfa).into(), window);
            }
        }
        if let Some(pose) = pose
            && let Some(rotation) = pose.rotation
        {
            let angle = rotation.to_radians();
            let needle = point(
                pose.position.x + px(40. * angle.cos()),
                pose.position.y + px(40. * angle.sin()),
            );
            paint_segment(pose.position, needle, 3., rgb(0xfb923c).into(), window);
            paint_dot(needle, 4., rgb(0xfb923c).into(), window);
        }
    }

    impl Render for PenExample {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let safe_area = window.insets().safe_area;
            let strokes = self.strokes.clone();
            let predicted = self.predicted.clone();
            let hover = self.hover;
            let pose = self.pose;
            div()
                .size_full()
                .flex()
                .pt(safe_area.top)
                .pb(safe_area.bottom)
                .pl(safe_area.left)
                .pr(safe_area.right)
                .bg(rgb(0x101622))
                .text_color(rgb(0xf5f7ff))
                .child(
                    div()
                        .id("canvas")
                        .flex_1()
                        .h_full()
                        .track_focus(&self.focus_handle)
                        .on_pen(cx.listener(Self::handle_pen))
                        .on_pen_gesture(
                            cx.listener(|this, event, _, cx| this.handle_gesture(event, cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.canvas_mouse.down += 1;
                                cx.notify();
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, _, _, cx| {
                            this.canvas_mouse.moved += 1;
                            cx.notify();
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.canvas_mouse.up += 1;
                                cx.notify();
                            }),
                        )
                        .on_scroll(cx.listener(|this, _, _, cx| {
                            this.canvas_mouse.scroll += 1;
                            cx.notify();
                        }))
                        .child(
                            canvas(
                                |_, _, _| {},
                                move |_, _, window, _| {
                                    paint_strokes(strokes, predicted, hover, pose, window)
                                },
                            )
                            .size_full(),
                        ),
                )
                .child(
                    div()
                        .w(px(420.))
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .bg(rgb(0x18202f))
                        .text_sm()
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(self.button(
                                    "claim",
                                    format!("Claim: {}", if self.claim { "on" } else { "off" }),
                                    |this| this.claim = !this.claim,
                                    cx,
                                ))
                                .child(self.button("clear", "Clear".into(), Self::clear, cx)),
                        )
                        .children(self.readout().into_iter().map(|line| div().child(line)))
                        .child(
                            div()
                                .id("fallback")
                                .h(px(160.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(0x4b5563))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.panel_mouse.down += 1;
                                        cx.notify();
                                    }),
                                )
                                .on_mouse_move(cx.listener(|this, _, _, cx| {
                                    this.panel_mouse.moved += 1;
                                    cx.notify();
                                }))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.panel_mouse.up += 1;
                                        cx.notify();
                                    }),
                                )
                                .on_scroll(cx.listener(|this, _, _, cx| {
                                    this.panel_mouse.scroll += 1;
                                    cx.notify();
                                }))
                                .child("Fallback area (no pen handler)"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .text_xs()
                                .text_color(rgb(0x9ca3af))
                                .children(
                                    self.log.iter().rev().map(|line| div().child(line.clone())),
                                ),
                        ),
                )
        }
    }

    pub fn main() {
        gpui_platform::application().run(|cx: &mut App| {
            gpui_ios::ios::set_status_bar_style(gpui_ios::StatusBarContentStyle::Light);
            cx.open_window(WindowOptions::default(), |window, cx| {
                cx.new(|cx| PenExample::new(window, cx))
            })
            .unwrap();
            cx.activate(true);
        });
    }
}

#[cfg(target_os = "ios")]
fn main() {
    app::main();
}

#[cfg(not(target_os = "ios"))]
fn main() {
    eprintln!("Build this example for an iOS target.");
}
