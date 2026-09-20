//! AppKit adapter for GPUI 0.2.2's inaccessible dispatch_event return type.
#![allow(unsafe_code, deprecated, unexpected_cfgs)]
use cocoa::{
    appkit::{NSApp, NSView, NSWindow},
    base::{id, nil},
    foundation::{NSPoint, NSRect},
};
use core_graphics::{
    event::{CGEvent, CGEventType, CGMouseButton, EventField},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::CGPoint,
};
use foreign_types::ForeignType;
use objc::{class, msg_send, sel, sel_impl};

// Called on the application main thread, outside any GPUI App/Window borrow.
// Only the fixture's own content view receives events; nothing is posted globally.
pub fn dispatch(y: f64, scroll: Option<i32>) -> Result<(), String> {
    unsafe {
        let windows: id = msg_send![NSApp(), windows];
        let count: usize = msg_send![windows, count];
        let mut window = nil;
        for i in 0..count {
            let candidate: id = msg_send![windows, objectAtIndex: i];
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
        for i in 0..count {
            let candidate: id = msg_send![children, objectAtIndex: i];
            if (*candidate).class().name() == "GPUIView" {
                view = candidate;
                break;
            }
        }
        if view == nil {
            return Err("GPUIView not found".into());
        }
        let height = NSView::frame(view).size.height;
        let number: isize = msg_send![window, windowNumber];
        let event: id = msg_send![class!(NSEvent), mouseEventWithType: 5_usize
            location: NSPoint::new(100., height - y) modifierFlags: 0_usize
            timestamp: 0_f64 windowNumber: number context: nil eventNumber: 0_isize
            clickCount: 0_isize pressure: 0_f32];
        if event == nil {
            return Err("cannot create native mouse event".into());
        }
        let _: () = msg_send![view, mouseMoved: event];
        if let Some(delta) = scroll {
            let source =
                CGEventSource::new(CGEventSourceStateID::Private).map_err(|_| "event source")?;
            let screens: id = msg_send![class!(NSScreen), screens];
            let screen: id = msg_send![screens, objectAtIndex: 0_usize];
            let screen_frame: NSRect = msg_send![screen, frame];
            let event = CGEvent::new_mouse_event(
                source,
                CGEventType::MouseMoved,
                CGPoint::new(100., screen_frame.size.height - height + y),
                CGMouseButton::Left,
            )
            .map_err(|_| "scroll event")?;
            event.set_type(CGEventType::ScrollWheel);
            event.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS, 1);
            event.set_integer_value_field(
                EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1,
                i64::from(delta),
            );
            // AppKit flips the unattached CGEvent's global Y about the primary screen;
            // compensate so GPUI receives the same local point as the mouse event.
            let native: id = msg_send![class!(NSEvent), eventWithCGEvent: event.as_ptr()];
            if native == nil {
                return Err("cannot create native scroll event".into());
            }
            let _: () = msg_send![view, scrollWheel: native];
        }
        Ok(())
    }
}
