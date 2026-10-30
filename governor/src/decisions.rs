//! Bounded store of recent decisions for GET /v1/decisions/:id and the operator resolve call.
//! An unbounded map grows forever under load (one entry per request), so the server keeps at most
//! `cap` decisions. When full, the oldest decision that is not a pending REVIEW is evicted first;
//! only when every stored decision is a pending REVIEW is the oldest of those evicted (its ledger
//! entry stays pending; the operator then gets 409 on resolve and must use the audit log).
use crate::engine::Decision;
use crate::verdict::Verdict;
use std::collections::{HashMap, VecDeque};

pub struct DecisionStore {
    cap: usize,
    order: VecDeque<String>,
    map: HashMap<String, Decision>,
}

impl DecisionStore {
    pub fn new(cap: usize) -> Self {
        DecisionStore { cap: cap.max(1), order: VecDeque::new(), map: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Decision> {
        self.map.get(id)
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.map.clear();
    }

    pub fn insert(&mut self, d: Decision) {
        let id = d.decision_id.clone();
        if self.map.insert(id.clone(), d).is_some() {
            self.order.retain(|x| x != &id);
        }
        self.order.push_back(id);
        while self.map.len() > self.cap {
            let victim = self
                .order
                .iter()
                .position(|k| self.map.get(k).map(|d| d.verdict != Verdict::Review).unwrap_or(true))
                .unwrap_or(0);
            if let Some(k) = self.order.remove(victim) {
                self.map.remove(&k);
            }
        }
    }
}
