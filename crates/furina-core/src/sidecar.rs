//! Client for the Python tool sidecar: spawns the process and speaks
//! newline-delimited JSON-RPC over stdio.

use furina_proto::{Event, RpcMessage, RpcNotification, RpcRequest};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>;

/// Event sink shared by background tasks (must be Send + Sync).
pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

pub struct Sidecar {
    stdin: ChildStdin,
    pending: Pending,
    next_id: AtomicU64,
    notify: Arc<dyn Fn(RpcNotification) + Send + Sync>,
    _child: Child,
}

impl Sidecar {
    pub async fn spawn(
        python: &str,
        pythonpath: &str,
        workspace: &str,
        events: Arc<dyn EventSink>,
    ) -> anyhow::Result<Self> {
        let mut child = Command::new(python)
            .args(["-m", "furina_tools.server"])
            .env("PYTHONPATH", pythonpath)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("sidecar: 无法获取 stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("sidecar: 无法获取 stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("sidecar: 无法获取 stderr"))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        let notify: Arc<dyn Fn(RpcNotification) + Send + Sync> =
            Arc::new(move |n: RpcNotification| {
                if n.method == "term.output" {
                    let p = &n.params;
                    events.emit(Event::ToolStream {
                        name: "term".into(),
                        stream: p["stream"].as_str().unwrap_or("").to_string(),
                        data: p["data"].as_str().unwrap_or("").to_string(),
                    });
                } else {
                    events.emit(Event::Log {
                        level: n.method.clone(),
                        message: n.params.to_string(),
                    });
                }
            });
        let reader_notify = notify.clone();
        let stderr_notify = notify.clone();

        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<RpcMessage>(trimmed) {
                    Ok(RpcMessage::Response(r)) => {
                        if let Some(tx) = reader_pending.lock().unwrap().remove(&r.id) {
                            let res = match (r.result, r.error) {
                                (Some(v), _) => Ok(v),
                                (None, Some(e)) => Err(format!("{}（code {}）", e.message, e.code)),
                                (None, None) => Err("空响应".into()),
                            };
                            let _ = tx.send(res);
                        }
                    }
                    Ok(RpcMessage::Notification(n)) => reader_notify(n),
                    _ => {}
                }
            }
            let dead = std::mem::take(&mut *reader_pending.lock().unwrap());
            for (_, tx) in dead {
                let _ = tx.send(Err("sidecar 进程已退出".into()));
            }
        });

        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    stderr_notify(RpcNotification {
                        method: "log".into(),
                        params: serde_json::json!({"level": "stderr", "message": trimmed}),
                    });
                }
            }
        });

        let mut this = Self {
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            notify,
            _child: child,
        };
        let res = this
            .call("initialize", serde_json::json!({"workspace_root": workspace}))
            .await?;
        if res.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            anyhow::bail!("sidecar initialize 失败: {res}");
        }
        Ok(this)
    }

    pub async fn call(&mut self, method: &str, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let req = RpcRequest { id, method: method.to_string(), params };
        let line = serde_json::to_string(&req)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        let res = tokio::time::timeout(std::time::Duration::from_secs(600), rx)
            .await
            .map_err(|_| anyhow::anyhow!("sidecar 调用超时: {method}"))??;
        res.map_err(|e| anyhow::anyhow!("{e}"))
    }

    pub fn notify_clone(&self) -> Arc<dyn Fn(RpcNotification) + Send + Sync> {
        self.notify.clone()
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self._child.start_kill();
    }
}
