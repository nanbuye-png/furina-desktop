//! Furina Agent core: agent loop, permission gateway, context management,
//! LLM client, and the Python sidecar client.

pub mod asr;
pub mod agent;
pub mod app;
pub mod config;
pub mod context;
pub mod gateway;
pub mod interject;
pub mod llm;
pub mod proxy;
pub mod sidecar;
pub mod state;
pub mod vision;
pub mod voice;
pub mod web;
pub mod web_cache;

pub use config::Config;
pub use asr::AsrClient;
pub use vision::VisionClient;
pub use voice::VoiceClient;
pub use web_cache::{WebCache, WebCacheEntry};
