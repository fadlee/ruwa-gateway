//! Bounded memory of recent messages, so replies can quote by id before the
//! persistent chat storage (Fase 3) exists. GOWA quotes from its storage with
//! `Conversation: <stored text>`; this keeps the same three fields.

use std::collections::{HashMap, VecDeque};

const CAPACITY: usize = 2000;

#[derive(Debug, Clone)]
pub struct RecentMessage {
    pub chat: String,
    pub sender: String,
    pub text: String,
}

#[derive(Default)]
pub struct RecentMessages {
    order: VecDeque<String>,
    by_id: HashMap<String, RecentMessage>,
}

impl RecentMessages {
    pub fn insert(&mut self, id: String, message: RecentMessage) {
        if self.by_id.insert(id.clone(), message).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.by_id.remove(&old);
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&RecentMessage> {
        self.by_id.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> RecentMessage {
        RecentMessage {
            chat: "c".into(),
            sender: "s".into(),
            text: "t".into(),
        }
    }

    #[test]
    fn evicts_oldest() {
        let mut r = RecentMessages::default();
        for i in 0..=CAPACITY {
            r.insert(i.to_string(), msg());
        }
        assert!(r.get("0").is_none());
        assert!(r.get(&CAPACITY.to_string()).is_some());
    }
}
