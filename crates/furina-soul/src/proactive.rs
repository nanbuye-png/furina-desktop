//! 主动性引擎：Time / Environment / Memory 三类触发器输出 ProactiveEvent。
//! 输出交给 Conversation Core（CLI），由其决定是否/何时渲染，不直接说话。

/// 主动行为事件（携带可直接渲染的一行消息）。
#[derive(Debug, Clone)]
pub struct ProactiveEvent {
    pub kind: String,
    pub priority: u8,
    pub message: String,
}
