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
    click_button(x, y, false)
}

pub(crate) fn right_click(x: f64, y: f64) -> Result<(), String> {
    click_button(x, y, true)
}

fn click_button(x: f64, y: f64, right: bool) -> Result<(), String> {
    unsafe {
        // Match the performance adapter: a fixture can render without being key.
        let windows: id = msg_send![NSApp(), windows];
        let count: usize = msg_send![windows, count];
        let mut window = nil;
        for index in 0..count {
            let candidate: id = msg_send![windows, objectAtIndex: index];
            let title: id = msg_send![candidate, title];
            let text: *const std::ffi::c_char = msg_send![title, UTF8String];
            if !text.is_null() && std::ffi::CStr::from_ptr(text).to_bytes() == b"Herdr" {
                window = candidate;
                break;
            }
        }
        if window == nil {
            return Err("fixture window not found".into());
        }
        let children: id = msg_send![window.contentView(), subviews];
        let count: usize = msg_send![children, count];
        let mut view = nil;
        for index in 0..count {
            let child: id = msg_send![children, objectAtIndex: index];
            if (*child).class().name() == "GPUIView" {
                view = child;
                break;
            }
        }
        if view == nil {
            return Err("fixture GPUIView missing".into());
        }
        let number: isize = msg_send![window, windowNumber];
        for kind in if right { [3_usize, 4] } else { [1_usize, 2] } {
            // NSEvent's convenience constructor reports buttonNumber=0 even for
            // right-click types. Build an explicit right-button CGEvent instead.
            let cg_event = if right {
                let source = CGEventSource::new(CGEventSourceStateID::Private)
                    .map_err(|_| "mouse event source")?;
                let screens: id = msg_send![class!(NSScreen), screens];
                let screen: id = msg_send![screens, objectAtIndex: 0_usize];
                let screen_frame: NSRect = msg_send![screen, frame];
                Some(
                    CGEvent::new_mouse_event(
                        source,
                        if kind == 3 {
                            CGEventType::RightMouseDown
                        } else {
                            CGEventType::RightMouseUp
                        },
                        CGPoint::new(
                            x,
                            screen_frame.size.height - NSView::frame(view).size.height + y,
                        ),
                        CGMouseButton::Right,
                    )
                    .map_err(|_| "right mouse event")?,
                )
            } else {
                None
            };
            let event: id = if let Some(event) = &cg_event {
                msg_send![class!(NSEvent), eventWithCGEvent: event.as_ptr()]
            } else {
                msg_send![class!(NSEvent), mouseEventWithType: kind
                    location: NSPoint::new(x, NSView::frame(view).size.height - y)
                    modifierFlags: 0_usize timestamp: 0_f64 windowNumber: number
                    context: nil eventNumber: 0_isize clickCount: 1_isize pressure: 1_f32]
            };
            if event == nil {
                return Err("cannot create fixture click".into());
            }
            let button: isize = msg_send![event, buttonNumber];
            if button != isize::from(right) {
                return Err(format!(
                    "fixture event has button {button}, expected right={right}"
                ));
            }
            if kind == 3 {
                let _: () = msg_send![view, rightMouseDown: event];
            } else if kind == 4 {
                let _: () = msg_send![view, rightMouseUp: event];
            } else if kind == 1 {
                let _: () = msg_send![view, mouseDown: event];
            } else {
                let _: () = msg_send![view, mouseUp: event];
            }
        }
        Ok(())
    }
}
