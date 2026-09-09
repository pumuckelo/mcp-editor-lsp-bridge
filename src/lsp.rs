use crate::protocol::{read_message, write_message};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, RwLock, oneshot},
};

#[derive(Default)]
pub struct AnalysisState {
    pub ready: bool,
    pub status: String,
    pub diagnostics: HashMap<String, Value>,
}
pub struct Lsp {
    writer: Mutex<ChildStdin>,
    child: Mutex<Child>,
    #[cfg(unix)]
    process_group: std::sync::Mutex<ProcessGroup>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>,
    next: AtomicU64,
    pub state: RwLock<AnalysisState>,
    pub capabilities: RwLock<Value>,
}
impl Lsp {
    pub async fn start(
        root: &Path,
        spec: crate::language::ServerSpec,
        environment: &HashMap<String, String>,
    ) -> Result<Arc<Self>> {
        let binary = &spec.command;
        let config = spec.settings;
        let section_name = spec.section;
        let server_status = spec.server_status;
        let mut command = Command::new(binary);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .args(&spec.args)
            .current_dir(root)
            .envs(environment)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Start {binary}"))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let server = Arc::new(Self {
            writer: Mutex::new(child.stdin.take().unwrap()),
            #[cfg(unix)]
            process_group: std::sync::Mutex::new(ProcessGroup(child.id().unwrap() as i32)),
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            state: RwLock::new(AnalysisState {
                status: "initializing".into(),
                ..Default::default()
            }),
            capabilities: RwLock::new(Value::Null),
        });
        let initialization = config.clone();
        let weak = Arc::downgrade(&server);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = match read_message(&mut reader).await {
                    Ok(Some(v)) => v,
                    _ => break,
                };
                let Some(server) = weak.upgrade() else { break };
                if let Some(method) = message["method"].as_str() {
                    let params = &message["params"];
                    if let Some(id) = message.get("id") {
                        let result = match method {
                            "workspace/configuration" => Value::Array(
                                params["items"]
                                    .as_array()
                                    .unwrap_or(&vec![])
                                    .iter()
                                    .map(|item| {
                                        let section = item["section"]
                                            .as_str()
                                            .unwrap_or("")
                                            .trim_start_matches(&format!("{section_name}."));
                                        if section.is_empty() || section == section_name {
                                            config.clone()
                                        } else {
                                            section.split('.').fold(&config, |v, k| &v[k]).clone()
                                        }
                                    })
                                    .collect(),
                            ),
                            "workspace/applyEdit" => {
                                json!({"applied":false,"failureReason":"Use the bridge apply_code_action tool"})
                            }
                            "workspace/diagnostic/refresh"
                            | "workspace/semanticTokens/refresh"
                            | "workspace/inlayHint/refresh"
                            | "window/workDoneProgress/create"
                            | "client/registerCapability"
                            | "client/unregisterCapability" => Value::Null,
                            _ => {
                                let _ = server.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unsupported client request"}})).await;
                                continue;
                            }
                        };
                        let _ = server
                            .send(json!({"jsonrpc":"2.0","id":id,"result":result}))
                            .await;
                    } else {
                        let mut state = server.state.write().await;
                        match method {
                            "textDocument/publishDiagnostics" => {
                                if let Some(uri) = params["uri"].as_str() {
                                    state.diagnostics.insert(uri.into(), params.clone());
                                }
                            }
                            "experimental/serverStatus" => {
                                state.ready = params["quiescent"].as_bool().unwrap_or(false)
                                    && params["health"] != "error";
                                state.status = params["message"]
                                    .as_str()
                                    .unwrap_or(if state.ready { "ready" } else { "indexing" })
                                    .into();
                            }
                            _ => {}
                        }
                    }
                } else if let Some(id) = message["id"].as_u64()
                    && let Some(sender) = server.pending.lock().await.remove(&id)
                {
                    let result = if let Some(error) = message.get("error") {
                        Err(anyhow::anyhow!("LSP: {error}"))
                    } else {
                        Ok(message["result"].clone())
                    };
                    let _ = sender.send(result);
                }
            }
            if let Some(server) = weak.upgrade() {
                let mut state = server.state.write().await;
                state.ready = false;
                state.status = "analyzer exited".into();
                for (_, sender) in server.pending.lock().await.drain() {
                    let _ = sender.send(Err(anyhow::anyhow!("Analyzer exited")));
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("language server: {line}");
            }
        });
        let uri = url::Url::from_directory_path(root)
            .map_err(|_| anyhow::anyhow!("Invalid workspace path"))?;
        let result = server.request("initialize", json!({"processId":std::process::id(),"rootUri":uri.as_str(),"workspaceFolders":[{"uri":uri.as_str(),"name":root.file_name().unwrap_or_default().to_string_lossy()}],"capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"configuration":true,"didChangeWatchedFiles":{"dynamicRegistration":true}},"textDocument":{"documentSymbol":{"hierarchicalDocumentSymbolSupport":true},"rename":{"prepareSupport":true},"diagnostic":{"dynamicRegistration":false,"relatedDocumentSupport":true},"publishDiagnostics":{"versionSupport":true},"codeAction":{"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","refactor","source"]}},"resolveSupport":{"properties":["edit"]}}},"window":{"workDoneProgress":true},"experimental":{"serverStatusNotification":true}},"initializationOptions":initialization})).await?;
        *server.capabilities.write().await = result["capabilities"].clone();
        if result["capabilities"]["positionEncoding"]
            .as_str()
            .is_some_and(|v| v != "utf-16")
        {
            bail!("Language server did not negotiate UTF-16 positions");
        }
        server.notify("initialized", json!({})).await?;
        if !server_status {
            let mut state = server.state.write().await;
            state.ready = true;
            state.status = "ready".into();
        }
        Ok(server)
    }
    pub async fn send(&self, value: Value) -> Result<()> {
        write_message(&mut *self.writer.lock().await, &value).await
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        for attempt in 0..4 {
            match self.request_once(method, params.clone()).await {
                Err(error) if attempt < 3 && error.to_string().contains("-32801") => {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                result => return result,
            }
        }
        unreachable!()
    }
    async fn request_once(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if let Err(error) = self
            .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(Duration::from_secs(90), rx).await {
            Ok(result) => result.context("LSP response channel closed")?,
            Err(_) => {
                self.pending.lock().await.remove(&id);
                let _ = self.notify("$/cancelRequest", json!({"id":id})).await;
                bail!("LSP request timed out: {method}")
            }
        }
    }
    pub async fn stop(&self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.request("shutdown", Value::Null),
        )
        .await;
        let _ = self.notify("exit", Value::Null).await;
        #[cfg(unix)]
        self.process_group.lock().unwrap().stop();
        let _ = self.child.lock().await.kill().await;
    }
    pub async fn pid(&self) -> Option<u32> {
        self.child.lock().await.id()
    }
}

// Wrappers may spawn the actual language server; stop the entire owned process group.
#[cfg(unix)]
struct ProcessGroup(i32);
#[cfg(unix)]
impl ProcessGroup {
    fn stop(&mut self) {
        if self.0 > 0 {
            // SAFETY: this group was created for our child, never the host shell.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
            self.0 = 0;
        }
    }
}
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.stop();
    }
}
