use crate::{lsp::Lsp, protocol::edit_text};
use anyhow::{Context, Result, bail};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::Mutex,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub analyzer: String,
    pub analyzer_settings: Value,
    pub environment: HashMap<String, String>,
    pub cargo_features: Vec<String>,
    pub cargo_target: Option<String>,
    pub all_features: bool,
    pub no_default_features: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            analyzer: "rust-analyzer".into(),
            analyzer_settings: json!({}),
            environment: HashMap::new(),
            cargo_features: vec![],
            cargo_target: None,
            all_features: false,
            no_default_features: false,
        }
    }
}
pub struct Core {
    pub sessions: Mutex<BTreeMap<PathBuf, Arc<Workspace>>>,
    pub config: Config,
}
impl Core {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(BTreeMap::new()),
            config,
        })
    }
    pub async fn workspace(&self, path: &str) -> Result<Arc<Workspace>> {
        if !Path::new(path).is_absolute() {
            bail!("Workspace must be an absolute directory path");
        }
        let root = std::fs::canonicalize(path).context("Workspace directory does not exist")?;
        if !root.join("Cargo.toml").is_file() {
            bail!("Workspace must contain Cargo.toml");
        }
        let mut sessions = self.sessions.lock().await;
        if let Some(workspace) = sessions.get(&root) {
            return Ok(workspace.clone());
        }
        let workspace = Workspace::start(root.clone(), self.config.clone()).await?;
        sessions.insert(root, workspace.clone());
        Ok(workspace)
    }
    pub async fn status(&self) -> Value {
        let sessions: Vec<_> = self.sessions.lock().await.values().cloned().collect();
        let mut items = vec![];
        for workspace in sessions {
            items.push(workspace.status().await);
        }
        json!({"workspaces":items})
    }
    pub async fn shutdown(&self) {
        for session in self.sessions.lock().await.values() {
            session.lsp.stop().await;
        }
    }
}
struct Document {
    text: String,
    disk: Option<String>,
    version: i64,
    epoch: u64,
    source: &'static str,
}
#[derive(Default, Serialize)]
struct CheckState {
    running: bool,
    generation: Option<u64>,
    success: Option<bool>,
    error: Option<String>,
    diagnostics: Vec<Value>,
}
struct State {
    documents: HashMap<String, Document>,
    disk_hashes: HashMap<String, u64>,
    generation: u64,
    companion: Option<String>,
    companion_seen: std::time::Instant,
    check: CheckState,
    watcher_error: Option<String>,
}
pub struct Workspace {
    instance: String,
    pub root: PathBuf,
    pub lsp: Arc<Lsp>,
    pub config: Config,
    state: Mutex<State>,
    check_lock: Mutex<()>,
    _watcher: std::sync::Mutex<Option<notify::RecommendedWatcher>>,
}
impl Workspace {
    async fn start(root: PathBuf, config: Config) -> Result<Arc<Self>> {
        let mut settings = config.analyzer_settings.clone();
        if !settings.is_object() {
            bail!("analyzer_settings must be an object");
        }
        settings["checkOnSave"] = json!(false);
        settings["cargo"]["features"] = if config.all_features {
            json!("all")
        } else {
            json!(config.cargo_features)
        };
        settings["cargo"]["noDefaultFeatures"] = json!(config.no_default_features);
        if let Some(target) = &config.cargo_target {
            settings["cargo"]["target"] = json!(target);
        }
        let lsp = Lsp::start(&root, &config.analyzer, settings, &config.environment).await?;
        let workspace = Arc::new(Self {
            instance: uuid::Uuid::new_v4().to_string(),
            root: root.clone(),
            lsp,
            config,
            state: Mutex::new(State {
                documents: HashMap::new(),
                disk_hashes: HashMap::new(),
                generation: 0,
                companion: None,
                companion_seen: std::time::Instant::now(),
                check: CheckState::default(),
                watcher_error: None,
            }),
            check_lock: Mutex::new(()),
            _watcher: std::sync::Mutex::new(None),
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        *workspace._watcher.lock().unwrap() = Some(watcher);
        let weak = Arc::downgrade(&workspace);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let Some(workspace) = weak.upgrade() else {
                    break;
                };
                match event {
                    Ok(event)
                        if event.kind.is_modify()
                            || event.kind.is_create()
                            || event.kind.is_remove() =>
                    {
                        let relevant = event.paths.iter().any(|p| workspace.relevant(p));
                        for path in event.paths {
                            if workspace.relevant(&path)
                                && let Err(error) = workspace.disk_changed(&path).await
                            {
                                workspace.state.lock().await.watcher_error =
                                    Some(error.to_string());
                            }
                        }
                        if relevant {
                            workspace.schedule_check().await;
                        }
                    }
                    Err(error) => {
                        workspace.state.lock().await.watcher_error = Some(error.to_string())
                    }
                    _ => {}
                }
            }
        });
        let weak = Arc::downgrade(&workspace);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(workspace) = weak.upgrade() else {
                    break;
                };
                let mut state = workspace.state.lock().await;
                if state.companion.is_some()
                    && state.companion_seen.elapsed() > Duration::from_secs(15)
                {
                    let _ = workspace.disconnect_locked(&mut state).await;
                }
            }
        });
        workspace.schedule_check().await;
        Ok(workspace)
    }
    async fn schedule_check(self: &Arc<Self>) {
        let generation = self.state.lock().await.generation;
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Some(workspace) = weak.upgrade() {
                let current = workspace.state.lock().await.generation;
                if current == generation {
                    let _ = workspace.check().await;
                }
            }
        });
    }
    fn relevant(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        !relative
            .components()
            .any(|p| p.as_os_str() == "target" || p.as_os_str() == ".git")
            && (path.extension().is_some_and(|x| x == "rs")
                || path.file_name().is_some_and(|x| {
                    x == "Cargo.toml"
                        || x == "Cargo.lock"
                        || x == "build.rs"
                        || x == "config.toml"
                        || x == "rust-toolchain.toml"
                }))
    }
    pub fn path(&self, input: &str) -> Result<PathBuf> {
        let path = if input.starts_with("file:") {
            url::Url::parse(input)?
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("Invalid file URI"))?
        } else {
            self.root.join(input)
        };
        let resolved = if path.exists() {
            path.canonicalize()?
        } else {
            path.parent()
                .context("Missing parent")?
                .canonicalize()?
                .join(path.file_name().context("Missing filename")?)
        };
        if !resolved.starts_with(&self.root) {
            bail!("Path outside workspace");
        }
        Ok(resolved)
    }
    pub fn uri(&self, path: &Path) -> Result<String> {
        Ok(url::Url::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("Invalid file path"))?
            .into())
    }
    async fn set_document(
        &self,
        state: &mut State,
        uri: &str,
        text: String,
        source: &'static str,
    ) -> Result<()> {
        if let Some(doc) = state.documents.get_mut(uri) {
            if doc.text != text {
                doc.version += 1;
                self.lsp.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":doc.version},"contentChanges":[{"text":text}]})).await?;
                doc.text = text;
                self.lsp.state.write().await.diagnostics.remove(uri);
            }
            doc.source = source;
        } else {
            let path = self.path(uri)?;
            self.lsp
                .notify(
                    "textDocument/didOpen",
                    json!({"textDocument":{"uri":uri,"languageId":"rust","version":1,"text":text}}),
                )
                .await?;
            state.documents.insert(
                uri.into(),
                Document {
                    text,
                    disk: std::fs::read_to_string(path).ok(),
                    version: 1,
                    epoch: 0,
                    source,
                },
            );
        }
        Ok(())
    }
    pub async fn open(&self, input: &str) -> Result<String> {
        let path = self.path(input)?;
        self.disk_changed(&path).await?;
        let uri = self.uri(&path)?;
        let mut state = self.state.lock().await;
        if !state.documents.contains_key(&uri) {
            self.set_document(&mut state, &uri, std::fs::read_to_string(path)?, "disk")
                .await?;
        }
        Ok(uri)
    }
    pub async fn disk_changed(&self, path: &Path) -> Result<()> {
        let uri = self.uri(path)?;
        let disk = std::fs::read_to_string(path).ok();
        let mut state = self.state.lock().await;
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        disk.hash(&mut hasher);
        let hash = hasher.finish();
        if state.disk_hashes.get(&uri) == Some(&hash) {
            return Ok(());
        }
        state.disk_hashes.insert(uri.clone(), hash);
        if let Some(doc) = state.documents.get_mut(&uri) {
            if doc.disk == disk {
                return Ok(());
            }
            doc.epoch += 1;
            doc.disk = disk.clone();
            if let Some(text) = &disk {
                self.set_document(&mut state, &uri, text.clone(), "disk")
                    .await?;
            } else {
                self.lsp
                    .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                    .await?;
                state.documents.remove(&uri);
                self.lsp.state.write().await.diagnostics.remove(&uri);
            }
        }
        state.generation += 1;
        self.lsp
            .notify(
                "workspace/didChangeWatchedFiles",
                json!({"changes":[{"uri":uri,"type":if disk.is_some(){2}else{3}}]}),
            )
            .await?;
        Ok(())
    }
    fn public_config(&self) -> Value {
        let mut value = serde_json::to_value(&self.config).unwrap();
        value["environment"] = json!(
            self.config
                .environment
                .keys()
                .map(|k| (k.clone(), "configured"))
                .collect::<BTreeMap<_, _>>()
        );
        value["analyzer_settings"]["checkOnSave"] = json!(false);
        value["compilerChecks"] = json!(
            "Shared core runs Cargo check after disk changes or explicit diagnostics(check=true)"
        );
        value
    }
    pub async fn status(&self) -> Value {
        let state = self.state.lock().await;
        let analysis = self.lsp.state.read().await;
        let documents: Vec<_> = state.documents.iter().map(|(uri,doc)| json!({"uri":uri,"source":doc.source,"version":doc.version,"epoch":doc.epoch})).collect();
        json!({"workspace":self.root,"analyzerPid":self.lsp.pid().await,"ready":analysis.ready,"status":analysis.status,"companion":state.companion,"generation":state.generation,"check":state.check,"checkFresh":state.check.generation == Some(state.generation) && !state.check.running && state.watcher_error.is_none(),"documents":documents,"config":self.public_config(),"watcherError":state.watcher_error})
    }
    pub async fn diagnostics(&self, check: bool) -> Result<Value> {
        if check {
            self.check().await?;
        }
        let state = self.state.lock().await;
        let analysis = self.lsp.state.read().await;
        let live: BTreeMap<_, _> = analysis
            .diagnostics
            .iter()
            .map(|(uri, params)| {
                let mut value = params.clone();
                value["matchesDocumentVersion"] = json!(
                    state
                        .documents
                        .get(uri)
                        .is_some_and(|d| params["version"].as_i64() == Some(d.version))
                );
                (uri, value)
            })
            .collect();
        Ok(
            json!({"ready":analysis.ready,"analysisStatus":analysis.status,"liveDiagnostics":live,"savedFileCheck":state.check,"checkFresh":state.check.generation == Some(state.generation) && !state.check.running && state.watcher_error.is_none(),"generation":state.generation,"note":"Compiler check applies to saved files only; live diagnostics may describe editor overlays. No completed check means unknown, not clean."}),
        )
    }
    pub async fn check(&self) -> Result<()> {
        let _guard = self.check_lock.lock().await;
        let generation = {
            let mut state = self.state.lock().await;
            if state.check.generation == Some(state.generation) && state.check.success.is_some() {
                return Ok(());
            }
            state.check.running = true;
            state.check.error = None;
            state.generation
        };
        let result = self.run_check().await;
        let mut state = self.state.lock().await;
        state.check.running = false;
        match result {
            Ok((success, diagnostics)) => {
                state.check.generation = Some(generation);
                state.check.success = Some(success);
                state.check.diagnostics = diagnostics;
            }
            Err(error) => {
                state.check.generation = None;
                state.check.success = None;
                state.check.error = Some(error.to_string());
            }
        }
        Ok(())
    }
    async fn run_check(&self) -> Result<(bool, Vec<Value>)> {
        let mut command = Command::new("cargo");
        command
            .current_dir(&self.root)
            .envs(&self.config.environment)
            .args([
                "check",
                "--workspace",
                "--all-targets",
                "--message-format=json",
            ]);
        if self.config.all_features {
            command.arg("--all-features");
        } else if !self.config.cargo_features.is_empty() {
            command.args(["--features", &self.config.cargo_features.join(",")]);
        }
        if self.config.no_default_features {
            command.arg("--no-default-features");
        }
        if let Some(target) = &self.config.cargo_target {
            command.args(["--target", target]);
        }
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stderr = child.stderr.take().unwrap();
        let errors = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut text = String::new();
            BufReader::new(stderr)
                .read_to_string(&mut text)
                .await
                .map(|_| text)
        });
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut diagnostics = vec![];
        while let Some(line) = lines.next_line().await? {
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && value["reason"] == "compiler-message"
            {
                diagnostics.push(value["message"].clone());
            }
        }
        let success = child.wait().await?.success();
        let stderr = errors.await??;
        if !success && diagnostics.is_empty() {
            bail!("cargo check failed: {stderr}");
        }
        Ok((success, diagnostics))
    }
    pub async fn sync(&self, event: SyncEvent) -> Result<Value> {
        // Refresh disk first, so queued editor events cannot undo an already observed disk mutation.
        if let Some(uri) = &event.uri {
            let path = self.path(uri)?;
            self.disk_changed(&path).await?;
        }
        let mut state = self.state.lock().await;
        if event.kind == "connect" {
            if state
                .companion
                .as_ref()
                .is_some_and(|id| id != &event.companion)
            {
                bail!("A Zed companion is already active for this workspace");
            }
            // A reconnect replaces the previous open-document set, including files
            // closed while the companion could not reach us.
            if state.companion.is_some() {
                self.disconnect_locked(&mut state).await?;
            }
            state.companion = Some(event.companion);
            state.companion_seen = std::time::Instant::now();
            return Ok(json!({"connected":true,"instance":self.instance}));
        }
        if state.companion.as_ref() != Some(&event.companion) {
            bail!("Companion is not connected");
        }
        state.companion_seen = std::time::Instant::now();
        if event.kind == "disconnect" {
            self.disconnect_locked(&mut state).await?;
            return Ok(json!({"connected":false}));
        }
        if event.kind == "heartbeat" {
            return Ok(json!({"connected":true}));
        }
        let uri = self.uri(&self.path(event.uri.as_deref().context("Missing uri")?)?)?;
        let epoch = state.documents.get(&uri).map_or(0, |d| d.epoch);
        if event.epoch != epoch {
            return Ok(
                json!({"accepted":false,"epoch":epoch,"reason":"Disk edit superseded editor snapshot"}),
            );
        }
        match event.kind.as_str() {
            "open" | "change" => {
                self.set_document(&mut state, &uri, event.text.context("Missing text")?, "zed")
                    .await?
            }
            "save" => {
                self.lsp
                    .notify("textDocument/didSave", json!({"textDocument":{"uri":uri}}))
                    .await?;
            }
            "close" => {
                if let Ok(text) = std::fs::read_to_string(self.path(&uri)?) {
                    self.set_document(&mut state, &uri, text, "disk").await?;
                } else {
                    self.lsp
                        .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                        .await?;
                    state.documents.remove(&uri);
                }
            }
            _ => bail!("Unknown sync event"),
        }
        Ok(json!({"accepted":true,"epoch":epoch}))
    }
    async fn disconnect_locked(&self, state: &mut State) -> Result<()> {
        state.companion = None;
        let uris: Vec<_> = state.documents.keys().cloned().collect();
        for uri in uris {
            if let Ok(text) = std::fs::read_to_string(self.path(&uri)?) {
                self.set_document(state, &uri, text, "disk").await?;
            } else {
                self.lsp
                    .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                    .await?;
                state.documents.remove(&uri);
            }
        }
        Ok(())
    }
    pub async fn apply_edit(&self, edit: &Value) -> Result<Value> {
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        if let Some(changes) = edit["changes"].as_object() {
            for (uri, edits) in changes {
                groups.insert(
                    uri.clone(),
                    edits.as_array().context("Invalid text edits")?.clone(),
                );
            }
        }
        if let Some(changes) = edit["documentChanges"].as_array() {
            for change in changes {
                if change.get("kind").is_some() {
                    bail!(
                        "File create/rename/delete code actions are not supported in this MVP; no edits applied"
                    );
                }
                groups
                    .entry(
                        change["textDocument"]["uri"]
                            .as_str()
                            .context("Missing document URI")?
                            .into(),
                    )
                    .or_default()
                    .extend(change["edits"].as_array().context("Missing edits")?.clone());
            }
        }
        let mut writes = vec![];
        for (uri, edits) in groups {
            let path = self.path(&uri)?;
            let state = self.state.lock().await;
            let text = state
                .documents
                .get(&uri)
                .map(|d| d.text.clone())
                .unwrap_or(std::fs::read_to_string(&path)?);
            writes.push((path, edit_text(&text, &edits)?));
        }
        let mut changed = vec![];
        for (path, text) in writes {
            std::fs::write(&path, text)?;
            self.disk_changed(&path).await?;
            changed.push(path);
        }
        Ok(json!({"applied":true,"files":changed}))
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SyncEvent {
    pub workspace: String,
    pub companion: String,
    pub kind: String,
    pub uri: Option<String>,
    pub text: Option<String>,
    #[serde(default)]
    pub epoch: u64,
}
