//! Deliver clicks only to the fixture's own AppKit content view.
#![allow(unsafe_code, deprecated, unexpected_cfgs)]
use cocoa::{
    appkit::{NSApp, NSView, NSWindow},
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

pub(crate) fn click(x: f64, y: f64) -> Result<(), String> {
    mouse_click(x, y, false)
}

pub(crate) fn right_click(x: f64, y: f64) -> Result<(), String> {
    mouse_click(x, y, true)
}

fn mouse_click(x: f64, y: f64, right: bool) -> Result<(), String> {
    unsafe {
        // The test owns one GPUI window. Do not depend on desktop keyboard focus.
        let windows: id = msg_send![NSApp(), windows];
        let window_count: usize = msg_send![windows, count];
        let mut window = nil;
        let mut view = nil;
        for index in 0..window_count {
            let candidate: id = msg_send![windows, objectAtIndex: index];
            let children: id = msg_send![candidate.contentView(), subviews];
            let count: usize = msg_send![children, count];
            for index in 0..count {
                let child: id = msg_send![children, objectAtIndex: index];
                if (*child).class().name() == "GPUIView" {
                    if view != nil {
                        return Err("multiple fixture GPUI windows".into());
                    }
                    view = child;
                    window = candidate;
                }
            }
        }
        if view == nil {
            return Err("fixture GPUIView missing".into());
        }
        let number: isize = msg_send![window, windowNumber];
        for kind in if right { [3_usize, 4] } else { [1_usize, 2] } {
            if right {
                // AppKit's mouseEventWithType reports button 0 even for right clicks.
                let source = CGEventSource::new(CGEventSourceStateID::Private)
                    .map_err(|_| "event source")?;
                let screens: id = msg_send![class!(NSScreen), screens];
                let screen: id = msg_send![screens, objectAtIndex: 0_usize];
                let frame: NSRect = msg_send![screen, frame];
                let event = CGEvent::new_mouse_event(
                    source,
                    if kind == 3 {
                        CGEventType::RightMouseDown
                    } else {
                        CGEventType::RightMouseUp
                    },
                    CGPoint::new(x, frame.size.height - NSView::frame(view).size.height + y),
                    CGMouseButton::Right,
                )
                .map_err(|_| "right mouse event")?;
                let native: id = msg_send![class!(NSEvent), eventWithCGEvent: event.as_ptr()];
                if native == nil {
                    return Err("cannot create native right mouse event".into());
                }
                if kind == 3 {
                    let _: () = msg_send![view, rightMouseDown: native];
                } else {
                    let _: () = msg_send![view, rightMouseUp: native];
                }
                continue;
            }
            let event: id = msg_send![class!(NSEvent), mouseEventWithType: kind
                location: NSPoint::new(x, NSView::frame(view).size.height - y)
                modifierFlags: 0_usize timestamp: 0_f64 windowNumber: number
                context: nil eventNumber: 0_isize clickCount: 1_isize pressure: 1_f32];
            if event == nil {
                return Err("cannot create fixture click".into());
            }
            if kind == 1 {
                let _: () = msg_send![view, mouseDown: event];
            } else {
                let _: () = msg_send![view, mouseUp: event];
            }
        }
        Ok(())
    }
}
