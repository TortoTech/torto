use std::cmp::Ordering;
use std::collections::BTreeMap;

use rebook_publication::{LocatorV1, SourceRange};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridTimestamp {
    pub wall_time_ms: u64,
    pub counter: u32,
    pub device_id: String,
}

impl Ord for HybridTimestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.wall_time_ms, self.counter, &self.device_id).cmp(&(
            other.wall_time_ms,
            other.counter,
            &other.device_id,
        ))
    }
}

impl PartialOrd for HybridTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub type VectorClock = BTreeMap<String, u64>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationState {
    pub id: String,
    pub book_id: String,
    pub ranges: Vec<SourceRange>,
    pub quote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: u64,
    pub updated_at: HybridTimestamp,
    #[serde(default)]
    pub clock: VectorClock,
    pub deleted_at: Option<HybridTimestamp>,
    pub origin_device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_of: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgressState {
    pub locator: LocatorV1,
    pub updated_at: HybridTimestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolDocument {
    pub version: u32,
    pub protocol: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookManifest {
    pub version: u32,
    pub book_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub file_name: String,
    pub content_path: String,
    pub content_sha256: String,
    pub content_length: u64,
    pub cover_path: Option<String>,
    pub added_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceBookEntry {
    pub book_id: String,
    pub present: bool,
    pub changed_at: HybridTimestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLibrary {
    pub version: u32,
    pub device_id: String,
    pub device_name: String,
    pub updated_at: HybridTimestamp,
    pub books: Vec<DeviceBookEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceBookState {
    pub version: u32,
    pub device_id: String,
    pub book_id: String,
    pub updated_at: HybridTimestamp,
    pub progress: Option<ProgressState>,
    pub annotations: Vec<AnnotationState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockOrder {
    Equal,
    Before,
    After,
    Concurrent,
}

pub fn compare_clocks(left: &VectorClock, right: &VectorClock) -> ClockOrder {
    let mut left_greater = false;
    let mut right_greater = false;
    for key in left.keys().chain(right.keys()) {
        let left_value = left.get(key).copied().unwrap_or_default();
        let right_value = right.get(key).copied().unwrap_or_default();
        left_greater |= left_value > right_value;
        right_greater |= right_value > left_value;
    }
    match (left_greater, right_greater) {
        (false, false) => ClockOrder::Equal,
        (false, true) => ClockOrder::Before,
        (true, false) => ClockOrder::After,
        (true, true) => ClockOrder::Concurrent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_clock_detects_causal_and_concurrent_changes() {
        let first = BTreeMap::from([("a".into(), 1)]);
        let later = BTreeMap::from([("a".into(), 2)]);
        let concurrent = BTreeMap::from([("a".into(), 1), ("b".into(), 1)]);

        assert_eq!(compare_clocks(&first, &later), ClockOrder::Before);
        assert_eq!(compare_clocks(&later, &first), ClockOrder::After);
        assert_eq!(compare_clocks(&later, &concurrent), ClockOrder::Concurrent);
        assert_eq!(compare_clocks(&first, &first), ClockOrder::Equal);
    }
}
