use crate::{
    language::{Language, TypeScriptBackend},
    lsp::Lsp,
    protocol::edit_text,
};
use anyhow::{Context, Result, bail};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
    pub typescript_analyzer: Option<String>,
    pub typescript_settings: Value,
    pub typescript_backend: TypeScriptBackend,
    pub typescript_language_server: Option<String>,
    pub typescript_language_server_settings: Value,
    pub vtsls_analyzer: Option<String>,
    pub vtsls_settings: Value,
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
            typescript_analyzer: None,
            typescript_settings: json!({}),
            typescript_backend: TypeScriptBackend::Auto,
            typescript_language_server: None,
            typescript_language_server_settings: json!({}),
            vtsls_analyzer: None,
            vtsls_settings: json!({}),
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
    disconnected: Mutex<BTreeSet<PathBuf>>,
    lifecycle: Mutex<()>,
    backends: Mutex<BTreeMap<PathBuf, TypeScriptBackend>>,
}
impl Core {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(BTreeMap::new()),
            config,
            disconnected: Mutex::new(BTreeSet::new()),
            lifecycle: Mutex::new(()),
            backends: Mutex::new(BTreeMap::new()),
        })
    }
    pub async fn workspace(&self, path: &str) -> Result<Arc<Workspace>> {
        self.attach(path, false).await
    }
    pub async fn connect(&self, path: &str) -> Result<Arc<Workspace>> {
        self.attach(path, true).await
    }
    async fn attach(&self, path: &str, explicit: bool) -> Result<Arc<Workspace>> {
        let _lifecycle = self.lifecycle.lock().await;
        if !Path::new(path).is_absolute() {
            bail!("Workspace must be an absolute directory path");
        }
        let root = std::fs::canonicalize(path).context("Workspace directory does not exist")?;
        Language::default_for(&root)?;
        if !explicit && self.disconnected.lock().await.contains(&root) {
            bail!(
                "Workspace disconnected; reconnect explicitly with bridge workspace-connect --workspace {}",
                root.display()
            );
        }
        let mut sessions = self.sessions.lock().await;
        if let Some(workspace) = sessions.get(&root) {
            return Ok(workspace.clone());
        }
        let mut config = self.config.clone();
        if let Some(backend) = self.backends.lock().await.get(&root) {
            config.typescript_backend = *backend;
        }
        let workspace = Workspace::start(root.clone(), config).await?;
        self.disconnected.lock().await.remove(&root);
        sessions.insert(root, workspace.clone());
        Ok(workspace)
    }
    /// Session overrides survive disconnect/reconnect, but not a core restart.
    pub async fn set_typescript_backend(
        &self,
        path: &str,
        backend: TypeScriptBackend,
    ) -> Result<Value> {
        let _lifecycle = self.lifecycle.lock().await;
        if !Path::new(path).is_absolute() {
            bail!("Workspace must be an absolute directory path");
        }
        let root = std::fs::canonicalize(path)?;
        let old = self
            .sessions
            .lock()
            .await
            .get(&root)
            .cloned()
            .context("Connect the workspace before changing its backend")?;
        if old.config.typescript_backend == backend {
            return Ok(old.status().await);
        }
        let _edit = old.edit_lock.lock().await;
        let snapshot = old.state.lock().await;
        let mut config = old.config.clone();
        config.typescript_backend = backend;
        let replacement = Workspace::start(root.clone(), config).await?;
        let restored = async {
            replacement.language_server("__bridge_backend_probe.ts").await?;
            for (uri, doc) in &snapshot.documents {
                let lsp = replacement.language_server(uri).await?;
                lsp.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":Language::document_id(&replacement.path(uri)?)?,"version":doc.version,"text":doc.text}})).await?;
            }
            let mut state = replacement.state.lock().await;
            state.documents = snapshot.documents.clone();
            state.companion = snapshot.companion.clone();
            state.companion_seen = snapshot.companion_seen;
            state.generation += 1;
            state.disk_hashes.clear();
            drop(state);
            // Reconcile disk writes that happened while the replacement started.
            for uri in snapshot.documents.keys() {
                replacement.disk_changed(&replacement.path(uri)?).await?;
            }
            Ok::<_, anyhow::Error>(())
        }.await;
        if let Err(error) = restored {
            replacement.stop().await;
            return Err(error);
        }
        drop(snapshot);
        old.stop().await;
        self.sessions
            .lock()
            .await
            .insert(root.clone(), replacement.clone());
        self.backends.lock().await.insert(root, backend);
        replacement.schedule_check().await;
        Ok(replacement.status().await)
    }
    pub async fn disconnect(&self, path: &str) -> Result<Value> {
        let _lifecycle = self.lifecycle.lock().await;
        if !Path::new(path).is_absolute() {
            bail!("Workspace must be an absolute directory path");
        }
        let root = std::fs::canonicalize(path).context("Workspace directory does not exist")?;
        self.disconnected.lock().await.insert(root.clone());
        if let Some(workspace) = self.sessions.lock().await.remove(&root) {
            workspace.stop().await;
        }
        Ok(json!({"workspace":root,"connected":false}))
    }
    pub async fn status(&self) -> Value {
        let sessions: Vec<_> = self.sessions.lock().await.values().cloned().collect();
        let mut items = vec![];
        for workspace in sessions {
            items.push(workspace.status().await);
        }
        json!({"workspaces":items,"disconnectedWorkspaces":self.disconnected.lock().await.iter().collect::<Vec<_>>()})
    }
    pub async fn shutdown(&self) {
        for session in self.sessions.lock().await.values() {
            session.stop().await;
        }
    }
}
#[derive(Clone)]
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
    refactor_disk: BTreeMap<PathBuf, u64>,
    generation: u64,
    companion: Option<String>,
    companion_seen: std::time::Instant,
    check: CheckState,
    watcher_error: Option<String>,
}
pub struct Workspace {
    instance: String,
    stopped: tokio::sync::watch::Sender<bool>,
    pub root: PathBuf,
    pub lsp: Arc<Lsp>,
    pub language: Language,
    servers: Mutex<BTreeMap<Language, Arc<Lsp>>>,
    pub config: Config,
    state: Mutex<State>,
    check_lock: Mutex<()>,
    pub edit_lock: Mutex<()>,
    _watcher: std::sync::Mutex<Option<notify::RecommendedWatcher>>,
}
impl Workspace {
    async fn start(root: PathBuf, config: Config) -> Result<Arc<Self>> {
        let language = Language::default_for(&root)?;
        let spec = language.spec(&root, &config).await?;
        let lsp = Lsp::start(&root, spec, &config.environment).await?;
        let workspace = Arc::new(Self {
            instance: uuid::Uuid::new_v4().to_string(),
            stopped: tokio::sync::watch::channel(false).0,
            root: root.clone(),
            lsp: lsp.clone(),
            language,
            servers: Mutex::new(BTreeMap::from([(language, lsp)])),
            config,
            state: Mutex::new(State {
                documents: HashMap::new(),
                disk_hashes: HashMap::new(),
                refactor_disk: BTreeMap::new(),
                generation: 0,
                companion: None,
                companion_seen: std::time::Instant::now(),
                check: CheckState::default(),
                watcher_error: None,
            }),
            check_lock: Mutex::new(()),
            edit_lock: Mutex::new(()),
            _watcher: std::sync::Mutex::new(None),
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        *workspace._watcher.lock().unwrap() = Some(watcher);
        let weak = Arc::downgrade(&workspace);
        let mut stopped = workspace.stopped.subscribe();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! { biased; _ = stopped.changed() => break, event = rx.recv() => match event { Some(event) => event, None => break } };
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
        let mut stopped = workspace.stopped.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! { biased; _ = stopped.changed() => break, _ = tokio::time::sleep(Duration::from_secs(5)) => {} }
                let Some(workspace) = weak.upgrade() else {
                    break;
                };
                let _edit = workspace.edit_lock.lock().await;
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
    pub async fn language_server(&self, input: &str) -> Result<Arc<Lsp>> {
        let language = Language::for_path(&self.path(input)?)?;
        let mut servers = self.servers.lock().await;
        self.ensure_running()?;
        if let Some(server) = servers.get(&language) {
            return Ok(server.clone());
        }
        let spec = language.spec(&self.root, &self.config).await?;
        let server = Lsp::start(&self.root, spec, &self.config.environment).await?;
        servers.insert(language, server.clone());
        drop(servers);
        self.state.lock().await.generation += 1;
        Ok(server)
    }
    async fn existing_server(&self, uri: &str) -> Result<Arc<Lsp>> {
        // Document operations call language_server before taking the document lock.
        let language = Language::for_path(&self.path(uri)?)?;
        self.servers
            .lock()
            .await
            .get(&language)
            .cloned()
            .context("Language server not started")
    }
    pub async fn workspace_symbols(&self, query: &str) -> Result<Value> {
        let servers: Vec<_> = self.servers.lock().await.values().cloned().collect();
        let mut symbols = vec![];
        for server in servers {
            let result = server
                .request("workspace/symbol", json!({"query":query}))
                .await?;
            symbols.extend(result.as_array().cloned().unwrap_or_default());
        }
        Ok(json!(symbols))
    }
    fn ensure_running(&self) -> Result<()> {
        if *self.stopped.borrow() {
            bail!("Workspace disconnected; reconnect explicitly with bridge workspace-connect");
        }
        Ok(())
    }
    pub async fn stop(&self) {
        self.stopped.send_replace(true);
        self._watcher.lock().unwrap().take();
        let _check = self.check_lock.lock().await;
        for server in self.servers.lock().await.values() {
            server.stop().await;
        }
    }
    async fn schedule_check(self: &Arc<Self>) {
        if self.ensure_running().is_err() {
            return;
        }
        let mut stopped = self.stopped.subscribe();
        let generation = self.state.lock().await.generation;
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::select! { biased; _ = stopped.changed() => return, _ = tokio::time::sleep(Duration::from_millis(500)) => {} }
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
        !relative.components().any(|p| {
            matches!(
                p.as_os_str().to_str(),
                Some("target" | ".git" | "node_modules" | "dist" | ".next")
            )
        }) && (Language::for_path(path).is_ok()
            || path.file_name().is_some_and(|x| {
                x == "tsconfig.json"
                    || x == "jsconfig.json"
                    || x == "package.json"
                    || x == "bun.lock"
                    || x == "package-lock.json"
                    || x == "pnpm-lock.yaml"
                    || x.to_string_lossy().starts_with("tsconfig.")
                    || x == "Cargo.toml"
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
        let lsp = self.existing_server(uri).await?;
        if let Some(doc) = state.documents.get_mut(uri) {
            if doc.text != text {
                doc.version += 1;
                let sync = lsp.capabilities.read().await["textDocumentSync"].clone();
                let incremental = sync.as_u64().or_else(|| sync["change"].as_u64()) == Some(2);
                let change = crate::protocol::document_change(&doc.text, &text, incremental);
                lsp.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":doc.version},"contentChanges":[change]})).await?;
                doc.text = text;
                lsp.state.write().await.diagnostics.remove(uri);
            }
            doc.source = source;
        } else {
            let path = self.path(uri)?;
            lsp
                .notify(
                    "textDocument/didOpen",
                    json!({"textDocument":{"uri":uri,"languageId":Language::document_id(&path)?,"version":1,"text":text}}),
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
        self.ensure_running()?;
        self.language_server(input).await?;
        let _edit = self.edit_lock.lock().await;
        let path = self.path(input)?;
        self.disk_changed_locked(&path).await?;
        let uri = self.uri(&path)?;
        let mut state = self.state.lock().await;
        if !state.documents.contains_key(&uri) {
            self.set_document(&mut state, &uri, std::fs::read_to_string(path)?, "disk")
                .await?;
        }
        Ok(uri)
    }
    pub async fn disk_changed(&self, path: &Path) -> Result<()> {
        let _edit = self.edit_lock.lock().await;
        self.disk_changed_locked(path).await
    }
    async fn disk_changed_locked(&self, path: &Path) -> Result<()> {
        self.ensure_running()?;
        let uri = self.uri(path)?;
        let server = if Language::for_path(path).is_ok() {
            self.servers
                .lock()
                .await
                .get(&Language::for_path(path)?)
                .cloned()
        } else {
            None
        };
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
            let lsp = server.as_ref().context("Missing document server")?;
            if doc.disk == disk {
                return Ok(());
            }
            doc.epoch += 1;
            doc.disk = disk.clone();
            if let Some(text) = &disk {
                self.set_document(&mut state, &uri, text.clone(), "disk")
                    .await?;
            } else {
                lsp.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                    .await?;
                state.documents.remove(&uri);
                lsp.state.write().await.diagnostics.remove(&uri);
            }
        }
        state.generation += 1;
        let servers: Vec<_> = self.servers.lock().await.values().cloned().collect();
        for lsp in servers {
            lsp.notify(
                "workspace/didChangeWatchedFiles",
                json!({"changes":[{"uri":uri,"type":if disk.is_some(){2}else{3}}]}),
            )
            .await?;
        }
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
        if self.language == Language::Rust {
            value["analyzer_settings"]["checkOnSave"] = json!(false);
        }
        value["compilerChecks"] = json!(
            "Shared core runs Cargo and/or workspace TypeScript checks for active languages after disk changes or explicit diagnostics(check=true)"
        );
        value
    }
    pub async fn status(&self) -> Value {
        let servers: Vec<_> = self
            .servers
            .lock()
            .await
            .iter()
            .map(|(language, server)| (*language, server.clone()))
            .collect();
        let mut analyzers = vec![];
        for (language, server) in servers {
            let analysis = server.state.read().await;
            analyzers.push(json!({"language":language,"backend":server.name,"pid":server.pid().await,"ready":analysis.ready,"status":analysis.status}));
        }
        let state = self.state.lock().await;
        let analysis = self.lsp.state.read().await;
        let documents: Vec<_> = state.documents.iter().map(|(uri,doc)| json!({"uri":uri,"source":doc.source,"version":doc.version,"epoch":doc.epoch})).collect();
        json!({"workspace":self.root,"language":self.language,"analyzers":analyzers,"analyzerPid":self.lsp.pid().await,"ready":analyzers.iter().all(|a| a["ready"] == true),"status":analysis.status,"companion":state.companion,"generation":state.generation,"check":state.check,"checkFresh":state.check.generation == Some(state.generation) && !state.check.running && state.watcher_error.is_none(),"documents":documents,"config":self.public_config(),"watcherError":state.watcher_error})
    }
    pub async fn diagnostics(&self, check: bool, path: Option<&str>) -> Result<Value> {
        self.ensure_running()?;
        // A path narrows live diagnostics to one document and pulls only that document.
        // The saved-file compiler check stays workspace-wide; a project check cannot be scoped per file.
        let target = match path {
            Some(path) => Some(self.open(path).await?),
            None => None,
        };
        if check {
            self.check().await?;
        }
        let documents: Vec<_> = self
            .state
            .lock()
            .await
            .documents
            .iter()
            .filter(|(uri, _)| {
                target
                    .as_deref()
                    .is_none_or(|target| target == uri.as_str())
            })
            .map(|(uri, doc)| (uri.clone(), doc.version))
            .collect();
        for (uri, version) in documents {
            let server = self.existing_server(&uri).await?;
            if !server.capabilities.read().await["diagnosticProvider"].is_null() {
                let result = server
                    .request(
                        "textDocument/diagnostic",
                        json!({"textDocument":{"uri":uri}}),
                    )
                    .await?;
                if result["kind"] == "full" {
                    server.state.write().await.diagnostics.insert(
                        uri.clone(),
                        json!({"uri":uri,"version":version,"diagnostics":result["items"]}),
                    );
                }
            }
        }
        let servers: Vec<_> = self.servers.lock().await.values().cloned().collect();
        let mut diagnostics = HashMap::new();
        for server in servers {
            diagnostics.extend(server.state.read().await.diagnostics.clone());
        }
        let state = self.state.lock().await;
        let analysis = self.lsp.state.read().await;
        let live: BTreeMap<_, _> = diagnostics
            .iter()
            .filter(|(uri, _)| {
                target
                    .as_deref()
                    .is_none_or(|target| target == uri.as_str())
            })
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
        let mut stopped = self.stopped.subscribe();
        self.ensure_running()?;
        let generation = {
            let mut state = self.state.lock().await;
            if state.check.generation == Some(state.generation) && state.check.success.is_some() {
                return Ok(());
            }
            state.check.running = true;
            state.check.error = None;
            state.generation
        };
        let result = tokio::select! { biased; _ = stopped.changed() => Err(anyhow::anyhow!("Workspace disconnected; check cancelled")), result = self.run_check() => result };
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
        let has_rust = self.servers.lock().await.contains_key(&Language::Rust);
        let has_typescript = self
            .servers
            .lock()
            .await
            .contains_key(&Language::TypeScript);
        let mut result = if has_rust {
            self.run_rust_check().await?
        } else {
            (true, vec![])
        };
        if has_typescript {
            let binary = crate::language::typescript_binary(&self.root, &self.config)?;
            let config = if self.root.join("tsconfig.json").is_file() {
                "tsconfig.json"
            } else if self.root.join("jsconfig.json").is_file() {
                "jsconfig.json"
            } else {
                bail!(
                    "Saved-file TypeScript check requires tsconfig.json or jsconfig.json at the workspace root; live file diagnostics remain available"
                )
            };
            let mut command = Command::new(binary);
            command
                .args(["--noEmit", "--pretty", "false", "--project", config])
                .current_dir(&self.root)
                .envs(&self.config.environment)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            #[cfg(unix)]
            command.process_group(0);
            let child = command.spawn()?;
            #[cfg(unix)]
            let mut process_group = CheckProcessGroup(child.id().unwrap() as i32);
            let output = child.wait_with_output().await?;
            #[cfg(unix)]
            {
                process_group.0 = 0;
            }
            result.0 &= output.status.success();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if !output.status.success() && text.trim().is_empty() {
                bail!(
                    "TypeScript check exited without diagnostics: {}",
                    output.status
                );
            }
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                result
                    .1
                    .push(json!({"source":"typescript","message":line,"rendered":line}));
            }
        }
        Ok(result)
    }
    async fn run_rust_check(&self) -> Result<(bool, Vec<Value>)> {
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
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        // Cancelling Cargo must also stop rustc/build scripts in its process group.
        #[cfg(unix)]
        let mut process_group = CheckProcessGroup(child.id().unwrap() as i32);
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
        #[cfg(unix)]
        {
            process_group.0 = 0;
        }
        let stderr = errors.await??;
        if !success && diagnostics.is_empty() {
            bail!("cargo check failed: {stderr}");
        }
        Ok((success, diagnostics))
    }
    pub async fn sync(&self, event: SyncEvent) -> Result<Value> {
        let _edit = self.edit_lock.lock().await;
        self.ensure_running()?;
        // Refresh disk first, so queued editor events cannot undo an already observed disk mutation.
        if let Some(uri) = &event.uri {
            self.language_server(uri).await?;
            let path = self.path(uri)?;
            self.disk_changed_locked(&path).await?;
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
                self.existing_server(&uri)
                    .await?
                    .notify("textDocument/didSave", json!({"textDocument":{"uri":uri}}))
                    .await?;
            }
            "close" => {
                if let Ok(text) = std::fs::read_to_string(self.path(&uri)?) {
                    self.set_document(&mut state, &uri, text, "disk").await?;
                } else {
                    self.existing_server(&uri)
                        .await?
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
                self.existing_server(&uri)
                    .await?
                    .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                    .await?;
                state.documents.remove(&uri);
            }
        }
        Ok(())
    }
    // Callers hold edit_lock across capture, the LSP request, preparation and commit.
    pub async fn snapshot(&self) -> Result<crate::refactor::Snapshot> {
        self.ensure_running()?;
        let uris: Vec<_> = self.state.lock().await.documents.keys().cloned().collect();
        for uri in uris {
            self.disk_changed_locked(&self.path(&uri)?).await?;
        }
        let root = self.root.clone();
        let disk =
            tokio::task::spawn_blocking(move || crate::refactor::disk_snapshot(&root)).await??;
        let mut state = self.state.lock().await;
        // Flush changed closed files too: the watcher may not have delivered them yet.
        let mut changes = vec![];
        for (path, hash) in &disk {
            if state.refactor_disk.get(path) != Some(hash) {
                changes.push(json!({"uri":self.uri(path)?,"type":2}));
            }
        }
        for path in state.refactor_disk.keys() {
            if !disk.contains_key(path) {
                changes.push(json!({"uri":self.uri(path)?,"type":3}));
            }
        }
        if !changes.is_empty() {
            state.generation += 1;
        }
        state.refactor_disk = disk.clone();
        let documents = state
            .documents
            .iter()
            .map(|(uri, doc)| {
                (
                    uri.clone(),
                    crate::refactor::DocumentStamp {
                        hash: crate::refactor::fingerprint(doc.text.as_bytes()),
                        version: doc.version,
                        epoch: doc.epoch,
                    },
                )
            })
            .collect();
        drop(state);
        if !changes.is_empty() {
            let servers: Vec<_> = self.servers.lock().await.values().cloned().collect();
            for server in servers {
                server
                    .notify(
                        "workspace/didChangeWatchedFiles",
                        json!({"changes":changes}),
                    )
                    .await?;
            }
        }
        Ok(crate::refactor::Snapshot {
            instance: self.instance.clone(),
            disk,
            documents,
        })
    }
    pub async fn verify_snapshot(&self, snapshot: &crate::refactor::Snapshot) -> Result<()> {
        let current = self.snapshot().await?;
        if current.instance != snapshot.instance
            || current.disk != snapshot.disk
            || current.documents != snapshot.documents
        {
            bail!(
                "Stale refactor: workspace files or editor buffers changed. Request a new rename/preview; no edits applied"
            );
        }
        Ok(())
    }
    pub async fn document_text(&self, uri: &str) -> Result<String> {
        self.state
            .lock()
            .await
            .documents
            .get(uri)
            .map(|d| d.text.clone())
            .context("Document is not open")
    }
    pub async fn prepare_edit(
        &self,
        edit: &Value,
        snapshot: &crate::refactor::Snapshot,
    ) -> Result<crate::refactor::PreparedEdit> {
        self.verify_snapshot(snapshot).await?;
        let state = self.state.lock().await;
        let mut files = vec![];
        let mut seen = BTreeSet::new();
        let mut bytes = 0;
        for (uri, group) in crate::refactor::text_edit_groups(edit)? {
            let crate::refactor::DocumentEdits { version, edits } = group;
            let path = self.path(&uri)?;
            if !seen.insert(path.clone()) {
                bail!("Duplicate file aliases in refactor; no edits applied");
            }
            let original_disk = std::fs::read_to_string(&path)?;
            if snapshot.disk.get(&path)
                != Some(&crate::refactor::fingerprint(original_disk.as_bytes()))
            {
                bail!(
                    "Stale or unsupported refactor target: {}; no edits applied",
                    path.display()
                );
            }
            let canonical_uri = self.uri(&path)?;
            let document = state.documents.get(&canonical_uri);
            if let Some(version) = version
                && document.map(|d| d.version) != Some(version)
            {
                bail!("Stale LSP document version; no edits applied");
            }
            let before = document
                .map(|d| d.text.clone())
                .unwrap_or_else(|| original_disk.clone());
            let after = edit_text(&before, &edits)?;
            bytes += original_disk.len() + before.len() + after.len();
            if bytes > crate::refactor::MAX_EDIT_BYTES {
                bail!("Refactor exceeds 16 MiB edit limit; no edits applied");
            }
            files.push(crate::refactor::FileEdit {
                path,
                original_disk,
                before,
                after,
                edits,
            });
        }
        Ok(crate::refactor::PreparedEdit { files })
    }
    pub async fn commit_edit(
        &self,
        edit: &crate::refactor::PreparedEdit,
        snapshot: &crate::refactor::Snapshot,
        verbose: bool,
    ) -> Result<Value> {
        self.verify_snapshot(snapshot).await?;
        // No awaits between final validation and file replacement.
        self.ensure_running()?;
        edit.commit()?;
        for file in &edit.files {
            if let Err(error) = self.disk_changed_locked(&file.path).await {
                bail!(
                    "Edits applied but analyzer synchronization failed: {error}. Inspect disk; do not blindly retry"
                );
            }
        }
        Ok(edit.receipt(&self.root, true, verbose))
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

#[cfg(unix)]
struct CheckProcessGroup(i32);
#[cfg(unix)]
impl Drop for CheckProcessGroup {
    fn drop(&mut self) {
        if self.0 > 0 {
            // SAFETY: this is the separate process group created for our Cargo child.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
}
