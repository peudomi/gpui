use super::window::{IosWindowState, WindowReference};
use gpui::{
    MouseExitEvent, MouseMoveEvent, PenAxes, PenEvent, PenGesture, PenGestureEvent,
    PenGesturePhase, PenPhase, PenSample, PenSampleId, PlatformInput, Point, TouchId, point, px,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_foundation::{NSArray, NSNumber, NSObject, NSObjectProtocol, NSProcessInfo, NSSet};
use objc2_ui_kit::{
    UIEvent, UIGestureRecognizer, UIGestureRecognizerDelegate, UIGestureRecognizerState,
    UIHoverGestureRecognizer, UIPencilHoverPose, UIPencilInteraction, UIPencilInteractionDelegate,
    UIPencilInteractionPhase, UIPencilInteractionSqueeze, UIPencilInteractionTap, UITouch,
    UITouchPhase, UITouchProperties, UITouchType, UIView,
};
use std::{cell::RefCell, collections::HashMap, time::Duration};

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIPenTarget"]
    #[ivars = WindowReference]
    pub(super) struct PenTarget;

    unsafe impl NSObjectProtocol for PenTarget {}

    impl PenTarget {
        #[unsafe(method(handleHover:))]
        fn handle_hover(&self, recognizer: &UIHoverGestureRecognizer) {
            self.ivars()
                .with_window(|window| window.pen().handle_hover(window, recognizer));
        }
    }

    unsafe impl UIGestureRecognizerDelegate for PenTarget {
        #[unsafe(method(gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:))]
        fn should_recognize_simultaneously(
            &self,
            _recognizer: &UIGestureRecognizer,
            _other: &UIGestureRecognizer,
        ) -> bool {
            true
        }
    }

    unsafe impl UIPencilInteractionDelegate for PenTarget {
        #[unsafe(method(pencilInteraction:didReceiveTap:))]
        fn did_receive_tap(&self, _interaction: &UIPencilInteraction, tap: &UIPencilInteractionTap) {
            self.ivars().with_window(|window| {
                window.pen().dispatch_gesture(
                    window,
                    PenGesture::DoubleTap,
                    tap.timestamp(),
                    tap.hoverPose(),
                )
            });
        }

        #[unsafe(method(pencilInteraction:didReceiveSqueeze:))]
        fn did_receive_squeeze(
            &self,
            _interaction: &UIPencilInteraction,
            squeeze: &UIPencilInteractionSqueeze,
        ) {
            let phase = match squeeze.phase() {
                UIPencilInteractionPhase::Began => PenGesturePhase::Began,
                UIPencilInteractionPhase::Changed => PenGesturePhase::Changed,
                UIPencilInteractionPhase::Ended => PenGesturePhase::Ended,
                _ => PenGesturePhase::Cancelled,
            };
            self.ivars().with_window(|window| {
                window.pen().dispatch_gesture(
                    window,
                    PenGesture::Squeeze(phase),
                    squeeze.timestamp(),
                    squeeze.hoverPose(),
                )
            });
        }
    }
);

/// Returns whether the recognizer belongs to the pen input, so the view lets it begin.
pub(super) fn owns(recognizer: &UIGestureRecognizer) -> bool {
    recognizer
        .delegate()
        .is_some_and(|delegate| delegate.isKindOfClass(PenTarget::class()))
}

#[derive(Default)]
struct PenState {
    next_id: u64,
    strokes: HashMap<usize, TouchId>,
    revisions: HashMap<isize, TouchId>,
    hover: Option<TouchId>,
}

impl PenState {
    fn issue_id(&mut self) -> TouchId {
        self.next_id += 1;
        TouchId(self.next_id)
    }
}

/// Apple Pencil input for one window: contact strokes from `touchesBegan` and friends,
/// hover from a pencil-only hover recognizer, and barrel gestures from a pencil interaction.
pub(super) struct PenInput {
    state: RefCell<PenState>,
    target: Retained<PenTarget>,
    interaction: Retained<UIPencilInteraction>,
}

impl PenInput {
    pub(super) fn new(view: &UIView, main_thread: MainThreadMarker) -> Self {
        let target = PenTarget::alloc(main_thread).set_ivars(WindowReference::default());
        let target: Retained<PenTarget> = unsafe { msg_send![super(target), init] };

        let target_object: &AnyObject = &target;
        // SAFETY: `PenTarget` implements `handleHover:` and lives as long as the window.
        let hover = unsafe {
            UIHoverGestureRecognizer::initWithTarget_action(
                UIHoverGestureRecognizer::alloc(main_thread),
                Some(target_object),
                Some(sel!(handleHover:)),
            )
        };
        hover.setAllowedTouchTypes(&NSArray::from_retained_slice(&[NSNumber::new_isize(
            UITouchType::Pencil.0,
        )]));
        hover.setCancelsTouchesInView(false);
        hover.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        view.addGestureRecognizer(&hover);

        let interaction = UIPencilInteraction::initWithDelegate(
            UIPencilInteraction::alloc(main_thread),
            ProtocolObject::from_ref(&*target),
        );
        view.addInteraction(ProtocolObject::from_ref(&*interaction));

        Self {
            state: RefCell::default(),
            target,
            interaction,
        }
    }

    /// Adds the pencil interaction again once the window is in a scene. An interaction
    /// added before that stays pending and never receives taps or squeezes.
    pub(super) fn attach_to_scene(&self, view: &UIView) {
        let interaction = ProtocolObject::from_ref(&*self.interaction);
        view.removeInteraction(interaction);
        view.addInteraction(interaction);
    }

    pub(super) fn set_window(&self, window: std::rc::Weak<IosWindowState>) {
        *self.target.ivars().0.borrow_mut() = window;
    }

    /// Delivers a pencil touch as a [`PenEvent`]. Returns whether an element claimed it;
    /// otherwise the caller hands the touch to the touch path.
    pub(super) fn handle_touch(
        &self,
        window: &IosWindowState,
        touch: &UITouch,
        event: Option<&UIEvent>,
    ) -> bool {
        let view = window.view();
        let key = touch as *const UITouch as usize;
        let phase = match touch.phase() {
            UITouchPhase::Began => PenPhase::Down,
            UITouchPhase::Moved | UITouchPhase::Stationary => PenPhase::Moved,
            UITouchPhase::Ended => PenPhase::Up,
            _ => PenPhase::Cancelled,
        };

        let pen_event = {
            let mut state = self.state.borrow_mut();
            let id = if phase == PenPhase::Down {
                state.revisions.clear();
                let id = state.issue_id();
                state.strokes.insert(key, id);
                id
            } else {
                match state.strokes.get(&key) {
                    Some(id) => *id,
                    None => {
                        let id = state.issue_id();
                        state.strokes.insert(key, id);
                        id
                    }
                }
            };

            let coalesced = event
                .and_then(|event| event.coalescedTouchesForTouch(touch))
                .map(|touches| touches.to_vec())
                .unwrap_or_default();
            let mut samples: Vec<PenSample> = coalesced
                .iter()
                .map(|touch| touch_sample(touch, view))
                .collect();
            if samples.is_empty() {
                samples.push(touch_sample(touch, view));
            }
            for sample in &samples {
                if let Some(sample_id) = sample.id {
                    state.revisions.insert(sample_id.0 as isize, id);
                }
            }

            let predicted: Vec<PenSample> = event
                .and_then(|event| event.predictedTouchesForTouch(touch))
                .map(|touches| {
                    touches
                        .to_vec()
                        .iter()
                        .map(|touch| PenSample {
                            provisional: PenAxes::empty(),
                            id: None,
                            ..touch_sample(touch, view)
                        })
                        .collect()
                })
                .unwrap_or_default();

            if matches!(phase, PenPhase::Up | PenPhase::Cancelled) {
                state.strokes.remove(&key);
            }

            PenEvent {
                id,
                device_id: None,
                phase,
                tip: Default::default(),
                barrel: false,
                in_contact: !matches!(phase, PenPhase::Up | PenPhase::Cancelled),
                modifiers: Default::default(),
                samples: samples.into_iter().collect(),
                predicted: predicted.into_iter().collect(),
            }
        };

        window.dispatch_pen_input(PlatformInput::Pen(pen_event))
    }

    /// Delivers late values for samples sent earlier with provisional axes.
    pub(super) fn handle_revisions(&self, window: &IosWindowState, touches: &NSSet<UITouch>) {
        let view = window.view();
        let mut revisions: Vec<(TouchId, bool, Vec<PenSample>)> = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            for touch in touches {
                let Some(index) = touch.estimationUpdateIndex() else {
                    continue;
                };
                let index = index.integerValue();
                let Some(id) = state.revisions.get(&index).copied() else {
                    continue;
                };
                let mut sample = touch_sample(&touch, view);
                if sample.provisional.is_empty() {
                    state.revisions.remove(&index);
                }
                sample.id = Some(PenSampleId(index as u64));
                let in_contact = state.strokes.values().any(|stroke| *stroke == id);
                match revisions.iter_mut().find(|(stroke, ..)| *stroke == id) {
                    Some((_, _, samples)) => samples.push(sample),
                    None => revisions.push((id, in_contact, vec![sample])),
                }
            }
        }

        for (id, in_contact, samples) in revisions {
            window.dispatch_pen_input(PlatformInput::Pen(PenEvent {
                id,
                phase: PenPhase::Revised,
                in_contact,
                samples: samples.into_iter().collect(),
                ..Default::default()
            }));
        }
    }

    fn handle_hover(&self, window: &IosWindowState, recognizer: &UIHoverGestureRecognizer) {
        let view = window.view();
        let location = recognizer.locationInView(Some(view));
        let position = point(px(location.x as f32), px(location.y as f32));
        let (phase, id) = {
            let mut state = self.state.borrow_mut();
            let hovering = matches!(
                recognizer.state(),
                UIGestureRecognizerState::Began | UIGestureRecognizerState::Changed
            );
            if hovering && !state.strokes.is_empty() {
                return;
            }
            match recognizer.state() {
                UIGestureRecognizerState::Began => {
                    let id = state.issue_id();
                    state.hover = Some(id);
                    (PenPhase::Entered, id)
                }
                UIGestureRecognizerState::Changed => {
                    let id = match state.hover {
                        Some(id) => id,
                        None => {
                            let id = state.issue_id();
                            state.hover = Some(id);
                            id
                        }
                    };
                    (PenPhase::Moved, id)
                }
                _ => match state.hover.take() {
                    Some(id) => (PenPhase::Exited, id),
                    None => return,
                },
            }
        };

        let sample = PenSample {
            position,
            pressure: 0.,
            tilt: Some(tilt(
                recognizer.altitudeAngle(),
                recognizer.azimuthAngleInView(Some(view)),
            )),
            rotation: Some(-recognizer.rollAngle().to_degrees() as f32),
            distance: Some(recognizer.zOffset() as f32),
            timestamp: now(),
            ..Default::default()
        };
        let claimed = window.dispatch_pen_input(PlatformInput::Pen(PenEvent {
            id,
            phase,
            samples: std::iter::once(sample).collect(),
            ..Default::default()
        }));
        if claimed {
            return;
        }

        let input = if phase == PenPhase::Exited {
            PlatformInput::MouseExited(MouseExitEvent {
                position,
                pressed_button: None,
                modifiers: Default::default(),
            })
        } else {
            PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Default::default(),
            })
        };
        window.dispatch_pen_input(input);
    }

    fn dispatch_gesture(
        &self,
        window: &IosWindowState,
        gesture: PenGesture,
        timestamp: f64,
        pose: Option<Retained<UIPencilHoverPose>>,
    ) {
        let hover = pose.map(|pose| {
            let location = pose.location();
            PenSample {
                position: point(px(location.x as f32), px(location.y as f32)),
                pressure: 0.,
                tilt: Some(tilt(pose.altitudeAngle(), pose.azimuthAngle())),
                rotation: Some(-pose.rollAngle().to_degrees() as f32),
                distance: Some(pose.zOffset() as f32),
                timestamp: Duration::from_secs_f64(timestamp),
                ..Default::default()
            }
        });
        window.dispatch_pen_input(PlatformInput::PenGesture(PenGestureEvent {
            gesture,
            device_id: None,
            timestamp: Duration::from_secs_f64(timestamp),
            hover,
        }));
    }
}

fn touch_sample(touch: &UITouch, view: &UIView) -> PenSample {
    let location = touch.preciseLocationInView(Some(view));
    let maximum_force = touch.maximumPossibleForce();
    let expecting = touch.estimatedPropertiesExpectingUpdates();
    let mut provisional = PenAxes::empty();
    if expecting.contains(UITouchProperties::Location) {
        provisional |= PenAxes::POSITION;
    }
    if expecting.contains(UITouchProperties::Force) {
        provisional |= PenAxes::PRESSURE;
    }
    if expecting.intersects(UITouchProperties::Azimuth | UITouchProperties::Altitude) {
        provisional |= PenAxes::TILT;
    }
    if expecting.contains(UITouchProperties::Roll) {
        provisional |= PenAxes::ROTATION;
    }
    let id = if provisional.is_empty() {
        None
    } else {
        touch
            .estimationUpdateIndex()
            .map(|index| PenSampleId(index.integerValue() as u64))
    };
    PenSample {
        position: point(px(location.x as f32), px(location.y as f32)),
        pressure: if maximum_force > 0. {
            (touch.force() / maximum_force).clamp(0., 1.) as f32
        } else {
            0.
        },
        tilt: Some(tilt(
            touch.altitudeAngle(),
            touch.azimuthAngleInView(Some(view)),
        )),
        rotation: Some(-touch.rollAngle().to_degrees() as f32),
        wheel: None,
        distance: None,
        timestamp: Duration::from_secs_f64(touch.timestamp()),
        provisional,
        id,
    }
}

fn tilt(altitude: f64, azimuth: f64) -> Point<f32> {
    let horizontal = altitude.cos();
    point(
        (azimuth.cos() * horizontal)
            .atan2(altitude.sin())
            .to_degrees() as f32,
        (azimuth.sin() * horizontal)
            .atan2(altitude.sin())
            .to_degrees() as f32,
    )
}

fn now() -> Duration {
    Duration::from_secs_f64(NSProcessInfo::processInfo().systemUptime())
}
