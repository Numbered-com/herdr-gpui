//! Exact-window, main-thread-only AppKit events for the isolated fixture.
#![allow(unsafe_code, deprecated, unexpected_cfgs)]
use cocoa::{
    base::{id, nil},
    foundation::{NSPoint, NSRect},
};
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

    pub(crate) fn make_key(&self) {
        unsafe {
            let _: () = msg_send![self.window, makeKeyWindow];
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
}

impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.view, release];
            let _: () = msg_send![self.window, release];
        }
    }
}
