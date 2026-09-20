//! Exact-window, main-thread-only AppKit events for the isolated fixture.
#![allow(unsafe_code, deprecated, unexpected_cfgs)]
use cocoa::{
    base::{id, nil},
    foundation::{NSPoint, NSRect},
};
use core_graphics::{
    event::{CGEvent, CGEventType, CGMouseButton},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::CGPoint,
};
use foreign_types::ForeignType;
use objc::{class, msg_send, sel, sel_impl};
use std::{marker::PhantomData, rc::Rc};

#[derive(Debug)]
pub(crate) struct Target {
    view: id,
    window: id,
    _main_thread: PhantomData<Rc<()>>,
}

impl Target {
    pub(crate) fn acquire(window: &gpui::Window) -> Result<Self, String> {
        let handle =
            raw_window_handle::HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
        let raw_window_handle::RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err("fixture is not AppKit".into());
        };
        // GPUI supplies this live view on the UI thread. Retain both objects only
        // across the update boundary: native callbacks reenter GPUI there.
        unsafe {
            let main: bool = msg_send![class!(NSThread), isMainThread];
            if !main {
                return Err("native fixture target requires main thread".into());
            }
            let view = handle.ns_view.as_ptr().cast();
            let window: id = msg_send![view, window];
            if window == nil {
                return Err("fixture view has no window".into());
            }
            let _: id = msg_send![view, retain];
            let _: id = msg_send![window, retain];
            Ok(Self {
                view,
                window,
                _main_thread: PhantomData,
            })
        }
    }

    pub(crate) fn is_key(&self) -> bool {
        unsafe { msg_send![self.window, isKeyWindow] }
    }

    pub(crate) fn click(&self, x: f64, y: f64) -> Result<(), String> {
        unsafe {
            let bounds: NSRect = msg_send![self.view, bounds];
            let flipped: bool = msg_send![self.view, isFlipped];
            let point = NSPoint::new(
                bounds.origin.x + x,
                bounds.origin.y + if flipped { y } else { bounds.size.height - y },
            );
            let location: NSPoint = msg_send![self.view, convertPoint: point toView: nil];
            let number: isize = msg_send![self.window, windowNumber];
            for kind in [1_usize, 2] {
                let event: id = msg_send![class!(NSEvent), mouseEventWithType: kind
                    location: location modifierFlags: 0_usize timestamp: 0_f64
                    windowNumber: number context: nil eventNumber: 0_isize
                    clickCount: 1_isize pressure: 1_f32];
                if event == nil {
                    return Err("cannot create fixture click".into());
                }
                if kind == 1 {
                    let _: () = msg_send![self.view, mouseDown: event];
                } else {
                    let _: () = msg_send![self.view, mouseUp: event];
                }
            }
        }
        Ok(())
    }

    pub(crate) fn right_click(&self, x: f64, y: f64) -> Result<(), String> {
        // CGEvent supplies the right-button number that mouseEventWithType omits.
        // Dispatch directly to the retained fixture view, never the key window.
        unsafe {
            let bounds: NSRect = msg_send![self.view, bounds];
            let flipped: bool = msg_send![self.view, isFlipped];
            let point = NSPoint::new(
                bounds.origin.x + x,
                bounds.origin.y + if flipped { y } else { bounds.size.height - y },
            );
            let location: NSPoint = msg_send![self.view, convertPoint: point toView: nil];
            let screens: id = msg_send![class!(NSScreen), screens];
            let screen: id = msg_send![screens, objectAtIndex: 0_usize];
            let frame: NSRect = msg_send![screen, frame];
            for kind in [CGEventType::RightMouseDown, CGEventType::RightMouseUp] {
                let source = CGEventSource::new(CGEventSourceStateID::Private)
                    .map_err(|_| "event source")?;
                let event = CGEvent::new_mouse_event(
                    source,
                    kind,
                    CGPoint::new(location.x, frame.size.height - location.y),
                    CGMouseButton::Right,
                )
                .map_err(|_| "right mouse event")?;
                let native: id = msg_send![class!(NSEvent), eventWithCGEvent: event.as_ptr()];
                if native == nil {
                    return Err("cannot create native right mouse event".into());
                }
                if matches!(kind, CGEventType::RightMouseDown) {
                    let _: () = msg_send![self.view, rightMouseDown: native];
                } else {
                    let _: () = msg_send![self.view, rightMouseUp: native];
                }
            }
        }
        Ok(())
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.view, release];
            let _: () = msg_send![self.window, release];
        }
    }
}
