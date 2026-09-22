//! Bounded, inert presentation data. Sound and navigation hints are not actions.
use herdr_client::protocol::{SemanticNotification, SemanticNotificationKind, ToastHerdrPosition};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(crate) const PENDING_LIMIT: usize = 8;
pub(crate) const VISIBLE_LIMIT: usize = 3;
pub(crate) const LIFETIME: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct Notice {
    pub title: String,
    pub body: Option<String>,
    pub kind: SemanticNotificationKind,
    pub position: ToastHerdrPosition,
    pub expires: Instant,
}

pub(crate) fn safe_text(text: &str, limit: usize) -> String {
    // Bound scanning as well as output, even for a payload made entirely of controls.
    text.chars().take(limit).filter(|c| {
        !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{2028}' | '\u{2029}')
    }).collect()
}

impl Notice {
    pub fn new(notification: SemanticNotification, now: Instant) -> Self {
        let title = safe_text(&notification.title, 160);
        Self {
            title: if title.trim().is_empty() {
                "Notification".into()
            } else {
                title
            },
            body: notification
                .body
                .map(|body| safe_text(&body, 512))
                .filter(|body| !body.trim().is_empty()),
            kind: notification.kind,
            position: notification
                .position
                .unwrap_or(ToastHerdrPosition::BottomRight),
            expires: now + LIFETIME,
        }
    }
}

#[derive(Default)]
pub(crate) struct Toasts {
    pub entries: VecDeque<(u64, Notice)>,
    next_id: u64,
}

impl Toasts {
    pub fn receive(&mut self, notices: impl IntoIterator<Item = Notice>) {
        for notice in notices {
            if self.entries.len() == VISIBLE_LIMIT {
                self.entries.pop_front();
            }
            self.entries.push_back((self.next_id, notice));
            self.next_id = self.next_id.wrapping_add(1);
        }
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(_, notice)| notice.expires > now);
        before != self.entries.len()
    }

    pub fn dismiss(&mut self, id: u64) {
        self.entries.retain(|(entry, _)| *entry != id);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn notification(title: &str) -> SemanticNotification {
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: title.into(),
            body: Some("Review needed".into()),
            sound: Some(herdr_client::protocol::SemanticNotificationSound::Request),
            agent: Some("untrusted".into()),
            workspace_id: Some("duplicate-id".into()),
            tab_id: None,
            pane_id: None,
            position: Some(ToastHerdrPosition::TopLeft),
        }
    }

    #[test]
    fn notifications_bound_untrusted_text_and_preserve_position() {
        let mut wire = notification(&"\u{1b}\n\u{202e}\u{2066}界".repeat(1000));
        wire.body = Some("界".repeat(1000));
        let notice = Notice::new(wire, Instant::now());
        assert!(notice.title.chars().count() <= 160);
        assert!(notice.title.chars().all(|c| c == '界'));
        assert_eq!(notice.body.as_ref().map(|b| b.chars().count()), Some(512));
        assert_eq!(notice.position, ToastHerdrPosition::TopLeft);
        assert_eq!(safe_text(&"\0".repeat(10000), 160), "");
        assert_eq!(
            Notice::new(notification("\n\u{202e}"), Instant::now()).title,
            "Notification"
        );
    }

    #[test]
    fn notifications_drop_oldest_expire_at_deadline_and_dismiss_independently() {
        let now = Instant::now();
        let mut toasts = Toasts::default();
        toasts.receive((0..20).map(|id| Notice::new(notification(&id.to_string()), now)));
        assert_eq!(toasts.entries.len(), VISIBLE_LIMIT);
        assert_eq!(toasts.entries[0].1.title, "17");
        assert!(!toasts.expire(now + LIFETIME - Duration::from_nanos(1)));
        toasts.dismiss(18);
        assert_eq!(toasts.entries.len(), 2);
        toasts.receive([Notice::new(
            notification("new"),
            now + Duration::from_secs(1),
        )]);
        assert!(toasts.expire(now + LIFETIME));
        assert_eq!(toasts.entries.len(), 1);
        assert_eq!(toasts.entries[0].1.title, "new");
        assert!(toasts.expire(now + LIFETIME + Duration::from_secs(1)));
        assert!(toasts.entries.is_empty());
    }
}
