//! Shared wire types for the Furina Agent: JSON-RPC protocol, LLM messages,
//! tool schemas, structured events, and scan results.

use serde::{Deserialize, Serialize};

/// Newline-delimited JSON-RPC request sent by the core to the Python sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcNotification {
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcMessage {
    Request(RpcRequest),
    Response(RpcResponse),
    Notification(RpcNotification),
}

/// LLM chat message in OpenAI/DeepSeek-compatible format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System { content: String },
    User { content: String },
    Assistant {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub function: ToolFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionCall {
    pub name: String,
    /// JSON-encoded argument string produced by the model.
    pub arguments: String,
}

/// Tool descriptor passed to the LLM (function-calling schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Structured event emitted by the core; the CLI/persona layer renders it.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    SessionStarted { workspace: String, model: String },
    StateChanged { state: String },
    Scan { project_type: String, test_command: String, summary: String },
    PlanProposed { text: String },
    Message { role: String, content: String },
    /// LLM 回复的增量文本块（用于流式打字机渲染与逐句语音），content 为本次增量。
    MessageDelta { content: String },
    ToolCall { name: String, summary: String },
    ToolResult { name: String, ok: bool, summary: String },
    ToolStream { name: String, stream: String, data: String },
    ApprovalRequired { kind: String, detail: String },
    ApprovalGranted { kind: String },
    ApprovalDenied { kind: String },
    /// 执行关键节点的人格化插话（LLM 生成，展示层专用；代替写死模板）。
    Interjection { text: String },
    Tokens { prompt: u64, completion: u64, total: u64 },
    Verify { passed: bool, detail: String },
    TestReport {
        command: String,
        framework: String,
        passed: bool,
        total: i64,
        failed: i64,
        summary: String,
    },
    Checkpoint {
        sequence: u32,
        steps: u32,
        tokens: u64,
        reason: String,
        summary: String,
    },
    ExperienceLearned { id: String, summary: String },
    SelfChangeProposed {
        id: String,
        summary: String,
        targets: Vec<String>,
        applicable: bool,
    },
    SelfChangeApplied { id: String, success: bool, summary: String },
    TaskRecoveryAvailable {
        task_id: String,
        goal: String,
        status: String,
        checkpoint_count: u32,
        steps: u32,
        updated_at_ms: u128,
    },
    TaskRecoveryResumed { task_id: String, checkpoint_count: u32, steps: u32 },
    TaskRecoveryDiscarded { task_id: String },
    DiagnosticExported { path: String },
    Done { success: bool, summary: String },
    Log { level: String, message: String },
}

/// Result of a project scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub project_type: String,
    pub language: String,
    pub test_command: String,
    pub manifests: Vec<String>,
    pub top_level: Vec<FileEntry>,
    pub total_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub kind: String,
    pub size: Option<u64>,
}

/// Aggregate outcome of one agent task run.
#[derive(Debug, Clone)]
pub struct TaskOutcome {
    pub success: bool,
    pub summary: String,
    pub steps: u32,
    pub repair_rounds: u32,
    pub total_tokens: u64,
    pub checkpoint_count: u32,
    pub stop_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_round_trip() {
        let req = RpcRequest {
            id: 7,
            method: "fs.read_file".into(),
            params: serde_json::json!({"path": "a.py"}),
        };
        let line = serde_json::to_string(&RpcMessage::Request(req)).unwrap();
        let parsed: RpcMessage = serde_json::from_str(&line).unwrap();
        match parsed {
            RpcMessage::Request(r) => {
                assert_eq!(r.id, 7);
                assert_eq!(r.params["path"], "a.py");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn notification_round_trip() {
        let n = RpcNotification {
            method: "term.output".into(),
            params: serde_json::json!({"requestId": 1, "stream": "stdout", "data": "x"}),
        };
        let line = serde_json::to_string(&RpcMessage::Notification(n)).unwrap();
        let parsed: RpcMessage = serde_json::from_str(&line).unwrap();
        assert!(matches!(parsed, RpcMessage::Notification(_)));
    }

    #[test]
    fn chat_message_round_trip() {
        let m = ChatMessage::Assistant {
            content: Some("hi".into()),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: ToolFunctionCall { name: "term.run".into(), arguments: "{}".into() },
            }]),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "term.run");
        let back: ChatMessage = serde_json::from_value(v).unwrap();
        assert!(matches!(back, ChatMessage::Assistant { .. }));
    }
}
