//! Furina Agent core: agent loop, permission gateway, context management,
//! LLM client, and the Python sidecar client.

pub mod asr;
pub mod audit;
pub mod agent;
pub mod app;
pub mod app_launcher;
pub mod config;
pub mod context;
pub mod diagnostics;
pub mod experience;
pub mod gateway;
pub mod interject;
pub mod interaction;
pub mod llm;
pub mod proxy;
pub mod self_inspect;
pub mod sidecar;
pub mod state;
pub mod task_journal;
pub mod vision;
pub mod voice;
pub mod web;
pub mod web_cache;

pub use config::Config;
pub use asr::AsrClient;
pub use vision::VisionClient;
pub use voice::{VoiceClient, VoiceSynthesisProfile};
pub use web_cache::{WebCache, WebCacheEntry};
