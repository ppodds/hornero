//! A singleton audit log. The caller is passed per call, from the `AuthUser` handler argument.

use std::collections::VecDeque;
use std::sync::Mutex;

use hornero::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    pub user: String,
    pub action: String,
    pub todo: i32,
}

/// Application-wide record of audit entries (a stand-in for an audit table or log sink).
#[derive(Default)]
pub struct AuditTrail(Mutex<VecDeque<AuditEntry>>);

impl AuditTrail {
    /// Only the most recent entries are kept: the trail lives as long as the server, so an
    /// unbounded `Vec` would grow with every request.
    const CAPACITY: usize = 1000;

    pub fn entries(&self) -> Vec<AuditEntry> {
        self.0.lock().unwrap().iter().cloned().collect()
    }

    fn push(&self, entry: AuditEntry) {
        let mut entries = self.0.lock().unwrap();
        if entries.len() == Self::CAPACITY {
            entries.pop_front();
        }
        entries.push_back(entry);
    }
}

/// Writes audit entries to the application-wide trail.
#[derive(Injectable)]
pub struct AuditLog {
    trail: Arc<AuditTrail>,
}

impl AuditLog {
    pub fn record(&self, user: &str, action: &str, todo: i32) {
        tracing::info!(user, action, todo, "audit");
        self.trail.push(AuditEntry {
            user: user.into(),
            action: action.into(),
            todo,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trail_keeps_only_the_most_recent_entries() {
        let trail = AuditTrail::default();
        for todo in 0..(AuditTrail::CAPACITY as i32 + 5) {
            trail.push(AuditEntry {
                user: "u".into(),
                action: "a".into(),
                todo,
            });
        }
        let entries = trail.entries();
        assert_eq!(entries.len(), AuditTrail::CAPACITY);
        assert_eq!(entries[0].todo, 5, "the oldest entries are dropped first");
    }
}
