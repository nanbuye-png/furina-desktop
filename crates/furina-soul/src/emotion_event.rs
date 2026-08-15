//! Structured history for meaningful and ordinary emotion transitions.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_EMOTION_EVENTS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EmotionEvent {
    pub timestamp: u128,
    pub source: String,
    pub trigger_id: Option<String>,
    pub cause: String,
    pub mood_before: String,
    pub mood_after: String,
    pub intensity_before: f64,
    pub intensity_after: f64,
    pub deltas: HashMap<String, f64>,
    pub trend: String,
    pub unresolved: bool,
    pub important: bool,
}

impl Default for EmotionEvent {
    fn default() -> Self {
        Self {
            timestamp: 0,
            source: "unknown".into(),
            trigger_id: None,
            cause: String::new(),
            mood_before: "calm".into(),
            mood_after: "calm".into(),
            intensity_before: 0.0,
            intensity_after: 0.0,
            deltas: HashMap::new(),
            trend: "stable".into(),
            unresolved: false,
            important: false,
        }
    }
}
