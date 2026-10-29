//! Temporary two-finger pan and pinch for iOS.
//!
//! GPUI's portable touch recognizer follows a single finger, so iOS apps get
//! no two-finger pan and no pinch. Until upstream GPUI recognizes multi-touch
//! gestures, this module installs UIKit's own recognizers on the window's view
//! and converts them into [`ScrollEvent`]s and [`PinchEvent`]s.
//!
//! When either recognizer begins, UIKit cancels the touches it took over, so
//! GPUI's single-finger recognizer abandons whatever it had started.
//!
//! To remove: delete this file, its `mod` line, the two action methods, the
//! `install` call and the `owns` check in `window.rs`. See
//! `gpui/25-ios-multitouch.md` in studio-docs.

use gpui::{
    Modifiers, PinchEvent, PlatformInput, Point, ScrollDelta, ScrollEvent, ScrollSource,
    TouchPhase, px,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_core_foundation::CGPoint;
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_ui_kit::{
    UIGestureRecognizer, UIGestureRecognizerDelegate, UIGestureRecognizerState,
    UIPanGestureRecognizer, UIPinchGestureRecognizer, UIScrollTypeMask, UIView,
};

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIMultitouchDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UIGestureRecognizerDelegate for Delegate {
        #[unsafe(method(gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:))]
        fn simultaneous(
            &self,
            _recognizer: &UIGestureRecognizer,
            other: &UIGestureRecognizer,
        ) -> bool {
            other.isKindOfClass(UIPanGestureRecognizer::class())
                || other.isKindOfClass(UIPinchGestureRecognizer::class())
        }
    }
);

thread_local! {
    static DELEGATE: std::cell::OnceCell<Retained<Delegate>> = const { std::cell::OnceCell::new() };
}

fn delegate(main_thread: MainThreadMarker) -> Retained<Delegate> {
    DELEGATE.with(|cell| {
        cell.get_or_init(|| unsafe { msg_send![Delegate::alloc(main_thread), init] })
            .clone()
    })
}

/// Whether `recognizer` was added by [`install`]. `MetalView` overrides
/// `gestureRecognizerShouldBegin:`, which UIKit asks for every recognizer on
/// the view, so it must let these through.
pub(super) fn owns(recognizer: &UIGestureRecognizer) -> bool {
    let Some(main_thread) = MainThreadMarker::new() else {
        return false;
    };
    let ours = delegate(main_thread);
    recognizer.delegate().is_some_and(|current| {
        std::ptr::addr_eq(Retained::as_ptr(&current), Retained::as_ptr(&ours))
    })
}

/// Adds the two-finger pan and pinch recognizers to `view`. `view` must
/// implement `handleTwoFingerPan:` and `handleTwoFingerPinch:`.
pub(super) fn install(view: &UIView, main_thread: MainThreadMarker) {
    let target: &AnyObject = view;
    let delegate = delegate(main_thread);
    // SAFETY: the caller guarantees `view` implements both actions, and the view owns the recognizers.
    let pan = unsafe {
        UIPanGestureRecognizer::initWithTarget_action(
            UIPanGestureRecognizer::alloc(main_thread),
            Some(target),
            Some(sel!(handleTwoFingerPan:)),
        )
    };
    pan.setMinimumNumberOfTouches(2);
    pan.setMaximumNumberOfTouches(2);
    pan.setAllowedScrollTypesMask(UIScrollTypeMask(0));
    pan.setDelaysTouchesEnded(false);
    pan.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    view.addGestureRecognizer(&pan);
    // SAFETY: as above.
    let pinch = unsafe {
        UIPinchGestureRecognizer::initWithTarget_action(
            UIPinchGestureRecognizer::alloc(main_thread),
            Some(target),
            Some(sel!(handleTwoFingerPinch:)),
        )
    };
    pinch.setDelaysTouchesEnded(false);
    pinch.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    view.addGestureRecognizer(&pinch);
}

fn phase(recognizer: &UIGestureRecognizer) -> Option<TouchPhase> {
    match recognizer.state() {
        UIGestureRecognizerState::Began => Some(TouchPhase::Started),
        UIGestureRecognizerState::Changed => Some(TouchPhase::Moved),
        UIGestureRecognizerState::Ended => Some(TouchPhase::Ended),
        UIGestureRecognizerState::Cancelled | UIGestureRecognizerState::Failed => {
            Some(TouchPhase::Cancelled)
        }
        _ => None,
    }
}

fn location(recognizer: &UIGestureRecognizer, view: &UIView) -> Point<gpui::Pixels> {
    let location = recognizer.locationInView(Some(view));
    Point::new(px(location.x as f32), px(location.y as f32))
}

/// Converts one step of the two-finger pan into a scroll.
pub(super) fn pan_event(
    recognizer: &UIPanGestureRecognizer,
    view: &UIView,
) -> Option<PlatformInput> {
    let touch_phase = phase(recognizer)?;
    let translation = recognizer.translationInView(Some(view));
    recognizer.setTranslation_inView(CGPoint::ZERO, Some(view));
    Some(PlatformInput::Scroll(ScrollEvent {
        position: location(recognizer, view),
        delta: ScrollDelta::Pixels(Point::new(
            px(translation.x as f32),
            px(translation.y as f32),
        )),
        modifiers: Modifiers::default(),
        touch_phase,
        source: ScrollSource::Touch,
    }))
}

/// Converts one step of the pinch into a zoom delta relative to the previous step.
pub(super) fn pinch_event(
    recognizer: &UIPinchGestureRecognizer,
    view: &UIView,
) -> Option<PlatformInput> {
    let phase = phase(recognizer)?;
    let delta = recognizer.scale() as f32 - 1.;
    recognizer.setScale(1.);
    Some(PlatformInput::Pinch(PinchEvent {
        position: location(recognizer, view),
        delta,
        modifiers: Modifiers::default(),
        phase,
    }))
}
