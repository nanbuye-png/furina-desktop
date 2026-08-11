//! Context manager: token estimation, truncation, and progressive
//! summarization of old turns.

use furina_proto::ChatMessage;

/// Rough token estimate: ~0.6 token per char (accounts for CJK-heavy text).
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() * 6 / 10).max(1)
}

pub fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max_chars).collect();
    s.push_str("\n…[截断]");
    s
}

fn message_content(m: &ChatMessage) -> Option<&str> {
    match m {
        ChatMessage::System { content }
        | ChatMessage::User { content }
        | ChatMessage::Tool { content, .. } => Some(content),
        ChatMessage::Assistant { content, .. } => content.as_deref(),
    }
}

fn content_mut(m: &mut ChatMessage) -> Option<&mut String> {
    match m {
        ChatMessage::System { content }
        | ChatMessage::User { content }
        | ChatMessage::Tool { content, .. } => Some(content),
        ChatMessage::Assistant { content, .. } => content.as_mut(),
    }
}

pub struct ContextManager {
    pub per_request_max_tokens: usize,
    pub keep_recent: usize,
    pub summary_max_chars: usize,
    pub message_max_chars: usize,
}

impl ContextManager {
    /// Fit the transcript under the per-request token budget:
    /// keep the system prompt, compress the oldest turns into a summary
    /// message, then truncate individual messages if still over budget.
    pub fn fit(&self, messages: &[ChatMessage]) -> Vec<ChatMessage> {
        if messages.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<ChatMessage> = Vec::new();
        let first_is_system = matches!(messages[0], ChatMessage::System { .. });
        let start = if first_is_system { 1 } else { 0 };
        if first_is_system {
            out.push(messages[0].clone());
        }
        let rest = &messages[start..];
        if rest.is_empty() {
            return out;
        }

        let mut keep = self.keep_recent.min(rest.len());
        // 工具组原子性：assistant(tool_calls) 与其 tool 响应必须整体保留在同一
        // 窗口内，否则 API 会报 "tool must follow tool_calls"。
        // 若截断边界落在 Tool 上，持续向更早消息扩展，直到包含对应的 assistant(tool_calls)。
        while keep < rest.len() {
            let boundary = rest.len() - keep;
            if !matches!(rest[boundary], ChatMessage::Tool { .. }) {
                break;
            }
            keep += 1;
            if boundary > 0
                && matches!(
                    rest[boundary - 1],
                    ChatMessage::Assistant { tool_calls: Some(_), .. }
                )
            {
                break;
            }
        }
        // 防御：若窗口仍以孤儿 Tool 开头（异常数据），把它们归入摘要，绝不直接发给 API。
        let mut recent_start = rest.len().saturating_sub(keep);
        while recent_start < rest.len() && matches!(rest[recent_start], ChatMessage::Tool { .. }) {
            recent_start += 1;
        }
        let recent = &rest[recent_start..];
        let middle = &rest[..recent_start];

        if !middle.is_empty() {
            let mut summary = String::from("以下是较早会话的压缩摘要（保留关键信息）：\n");
            for m in middle {
                if let Some(c) = message_content(m) {
                    summary.push_str(&truncate_text(c, 1000));
                    summary.push('\n');
                }
            }
            out.push(ChatMessage::User { content: truncate_text(&summary, self.summary_max_chars) });
        }
        out.extend(recent.iter().cloned());

        // 仍超预算则逐条截断
        let mut total: usize = out.iter().map(|m| estimate_tokens(message_content(m).unwrap_or_default())).sum();
        let mut idx = 0;
        while total > self.per_request_max_tokens && idx < out.len() {
            if let Some(c) = content_mut(&mut out[idx]) {
                let old = estimate_tokens(c);
                *c = truncate_text(c, self.message_max_chars);
                let new = estimate_tokens(c);
                total = total.saturating_sub(old).saturating_add(new);
            }
            idx += 1;
        }
        out
    }
}

/// 移除转录末尾的"未完成回合"：如果末尾存在声明了 tool_calls 却没有完整工具响应的
/// assistant（例如被 Ctrl+C 中断），或残留孤立的 tool 消息，回退到最后一个干净边界，
/// 避免把非法序列发给 LLM。完整的历史回合不受影响。
pub fn trim_incomplete_turn(messages: &mut Vec<ChatMessage>) {
    loop {
        let mut i = messages.len();
        let mut tool_count = 0usize;
        let mut cut: Option<usize> = None;
        while i > 0 {
            i -= 1;
            match &messages[i] {
                ChatMessage::Tool { .. } => tool_count += 1,
                ChatMessage::Assistant { tool_calls: Some(calls), .. } => {
                    if tool_count != calls.len() {
                        cut = Some(i);
                    }
                    break;
                }
                _ => {
                    // 遇到非工具消息但后面还堆着 tool 消息 → 孤儿 tool
                    if tool_count > 0 {
                        cut = Some(i + 1);
                    }
                    break;
                }
            }
        }
        match cut {
            Some(idx) => messages.truncate(idx),
            None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use furina_proto::ToolCall;

    fn msg(role: &str, content: &str) -> ChatMessage {
        match role {
            "system" => ChatMessage::System { content: content.into() },
            "user" => ChatMessage::User { content: content.into() },
            "tool" => ChatMessage::Tool { tool_call_id: "c".into(), content: content.into() },
            _ => ChatMessage::Assistant { content: Some(content.into()), tool_calls: None },
        }
    }

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            r#type: "function".into(),
            function: furina_proto::ToolFunctionCall { name: "fs.read_file".into(), arguments: "{}".into() },
        }
    }

    #[test]
    fn estimate_counts_chars() {
        assert!(estimate_tokens("abc") >= 1);
        assert_eq!(estimate_tokens(""), 1);
    }

    #[test]
    fn truncate_marks_cut() {
        let t = truncate_text("一二三四五六七八九十", 5);
        assert!(t.contains("[截断]"));
        assert!(t.chars().count() <= 5 + 10);
    }

    #[test]
    fn fit_small_transcript_untouched() {
        let cm = ContextManager {
            per_request_max_tokens: 10_000,
            keep_recent: 20,
            summary_max_chars: 10_000,
            message_max_chars: 6_000,
        };
        let msgs = vec![msg("system", "sys"), msg("user", "hi"), msg("assistant", "yo")];
        let out = cm.fit(&msgs);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn fit_summarizes_old_turns() {
        let cm = ContextManager {
            per_request_max_tokens: 1_000,
            keep_recent: 2,
            summary_max_chars: 1_000,
            message_max_chars: 6_000,
        };
        let mut msgs = vec![msg("system", "sys")];
        for i in 0..30 {
            msgs.push(msg("user", &format!("turn {i} {}", "x".repeat(200))));
            msgs.push(msg("assistant", "ok"));
        }
        let out = cm.fit(&msgs);
        assert!(out.len() < msgs.len());
        assert!(matches!(out[0], ChatMessage::System { .. }));
        assert!(out.iter().any(|m| matches!(m, ChatMessage::User { content } if content.contains("压缩摘要"))));
    }

    #[test]
    fn fit_keeps_tool_pairs_together() {
        let cm = ContextManager {
            per_request_max_tokens: 100_000,
            keep_recent: 2,
            summary_max_chars: 1_000,
            message_max_chars: 6_000,
        };
        let mut msgs = vec![msg("system", "sys"), msg("user", "task")];
        msgs.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call("c1")]),
        });
        msgs.push(msg("tool", "result"));
        let out = cm.fit(&msgs);
        let last = out.len() - 1;
        assert!(matches!(out[last], ChatMessage::Tool { .. }));
        assert!(matches!(out[last - 1], ChatMessage::Assistant { tool_calls: Some(_), .. }));
    }

    #[test]
    fn fit_keeps_multi_tool_group_together() {
        let cm = ContextManager {
            per_request_max_tokens: 100_000,
            keep_recent: 2,
            summary_max_chars: 1_000,
            message_max_chars: 6_000,
        };
        let mut msgs = vec![msg("system", "sys"), msg("user", "task")];
        // 填充旧消息，把截断边界逼到工具组中间
        for i in 0..10 {
            msgs.push(msg("user", &format!("old turn {i}")));
            msgs.push(msg("assistant", "ok"));
        }
        msgs.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call("c1"), tool_call("c2"), tool_call("c3")]),
        });
        for i in 0..3 {
            msgs.push(msg("tool", &format!("result {i}")));
        }
        let out = cm.fit(&msgs);
        let last = out.len() - 1;
        // 完整工具组必须整体保留：assistant(tool_calls) 在其 3 条 tool 响应之前
        assert!(matches!(
            &out[last - 3],
            ChatMessage::Assistant { tool_calls: Some(calls), .. } if calls.len() == 3
        ));
        for k in 0..3 {
            assert!(matches!(out[last - 2 + k], ChatMessage::Tool { .. }));
        }
        // 全文无孤儿 tool：每个 Tool 都必须有尚未被响应的 tool_calls 可对应
        let mut pending = 0usize;
        for m in &out {
            match m {
                ChatMessage::Assistant { tool_calls: Some(calls), .. } => pending += calls.len(),
                ChatMessage::Tool { .. } => {
                    assert!(pending > 0, "存在孤立 tool 消息");
                    pending -= 1;
                }
                _ => pending = 0,
            }
        }
    }

    #[test]
    fn trim_keeps_complete_tool_group() {
        let mut msgs = vec![msg("user", "task")];
        msgs.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call("c1"), tool_call("c2")]),
        });
        msgs.push(msg("tool", "r1"));
        msgs.push(msg("tool", "r2"));
        let before = msgs.len();
        trim_incomplete_turn(&mut msgs);
        assert_eq!(msgs.len(), before);
    }

    #[test]
    fn trim_removes_incomplete_tool_group() {
        let mut msgs = vec![msg("user", "task")];
        msgs.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call("c1"), tool_call("c2")]),
        });
        msgs.push(msg("tool", "r1")); // 缺 c2 的响应
        trim_incomplete_turn(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], ChatMessage::User { .. }));
    }

    #[test]
    fn trim_removes_dangling_tool_calls_assistant() {
        let mut msgs = vec![msg("user", "task")];
        msgs.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call("c1")]),
        });
        trim_incomplete_turn(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], ChatMessage::User { .. }));
    }

    #[test]
    fn trim_removes_orphan_tool_messages() {
        let mut msgs = vec![msg("user", "task"), msg("tool", "orphan")];
        trim_incomplete_turn(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], ChatMessage::User { .. }));
    }

    #[test]
    fn trim_handles_stacked_incomplete_turns() {
        let mut msgs = vec![msg("user", "task")];
        msgs.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call("c1"), tool_call("c2")]),
        });
        msgs.push(msg("tool", "r1"));
        msgs.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![tool_call("c3")]),
        });
        trim_incomplete_turn(&mut msgs);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], ChatMessage::User { .. }));
    }
}
