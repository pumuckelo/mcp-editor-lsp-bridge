//! Transport-independent inbound port and semantic application service.
use crate::{
    core::{Core, Workspace},
    output,
    refactor::{PreparedEdit, Snapshot},
    symbols,
};
use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}
macro_rules! input {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Debug, Serialize, Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $(pub $field: $ty),* }
    };
}
input!(BackendInput { workspace: String, backend: crate::language::TypeScriptBackend, verbose: Option<bool> });
input!(ConnectionInput { workspace: String, verbose: Option<bool> });
input!(StatusInput { workspace: Option<String>, verbose: Option<bool> });
input!(WorkspaceInput {
    workspace: String,
    query: String,
    verbose: Option<bool>
});
input!(DocumentInput {
    workspace: String,
    path: String,
    verbose: Option<bool>
});
input!(PositionInput {
    workspace: String,
    path: String,
    line: Option<u32>,
    character: Option<u32>,
    symbol: Option<String>
});
input!(DiagnosticsInput { workspace: String, check: Option<bool>, verbose: Option<bool> });
input!(RenameInput { workspace: String, path: String, line: Option<u32>, character: Option<u32>, symbol: Option<String>, new_name: String, apply: Option<bool>, preview: Option<bool>, verbose: Option<bool> });
input!(PlanInput { workspace: String, plan_id: String, verbose: Option<bool> });
input!(ActionsInput { workspace: String, path: String, line: u32, character: u32, end: Option<Position> });
input!(ApplyInput {
    workspace: String,
    action_id: String,
    verbose: Option<bool>
});

#[derive(Debug, Serialize)]
pub struct OperationInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}
macro_rules! operations {
    ($( $variant:ident($input:ty) => ($name:literal, $description:literal) ),* $(,)?) => {
        #[derive(Debug, Serialize, Deserialize)]
        #[serde(tag = "operation", content = "arguments", deny_unknown_fields)]
        pub enum Request { $(#[serde(rename = $name)] $variant($input)),* }
        impl Request {
            pub fn decode(name: &str, arguments: Value) -> Result<Self, ApiError> {
                let request: Self = serde_json::from_value(json!({"operation":name,"arguments":arguments}))
                    .map_err(|error| ApiError::invalid(error.to_string()))?;
                request.validate()?;
                Ok(request)
            }
        }
        pub fn operations() -> Vec<OperationInfo> {
            vec![$(OperationInfo { name: $name, description: $description, input_schema: serde_json::to_value(schema_for!($input)).expect("serializable schema") }),*]
        }
    };
}
operations! {
    Backend(BackendInput) => ("workspace_backend", "Select TypeScript backend and restart a connected workspace, preserving editor buffers. Session-only; use config for a persistent default."),
    Connect(ConnectionInput) => ("workspace_connect", "Explicitly connect or resume a workspace analyzer."),
    Disconnect(ConnectionInput) => ("workspace_disconnect", "Stop a workspace analyzer and checks; block automatic reattachment until explicitly connected."),
    Status(StatusInput) => ("workspace_status", "List sessions, or attach a workspace and show analyzer, companion and check state."),
    WorkspaceSymbols(WorkspaceInput) => ("workspace_symbols", "Find symbols by query in the workspace."),
    DocumentSymbols(DocumentInput) => ("document_symbols", "List symbols in a supported source file."),
    Definition(PositionInput) => ("definition", "Find a symbol definition. Positions are zero-based UTF-16."),
    References(PositionInput) => ("references", "Find symbol references including declaration. Positions are zero-based UTF-16."),
    Hover(PositionInput) => ("hover", "Get type and documentation. Positions are zero-based UTF-16."),
    Diagnostics(DiagnosticsInput) => ("diagnostics", "Get live diagnostics and saved-file check state. check=true awaits shared saved-file compiler checks. Empty live diagnostics alone never mean clean."),
    Rename(RenameInput) => ("rename", "Rename and write edits by default. preview=true returns a guarded plan; apply=false is a legacy preview alias. Select by symbol or zero-based UTF-16 position."),
    ApplyRename(PlanInput) => ("apply_rename", "Apply exactly an unexpired preview plan, rejecting stale files or editor buffers."),
    CodeActions(ActionsInput) => ("code_actions", "List code actions at a zero-based UTF-16 position/range. Use action_id with apply_code_action."),
    ApplyCodeAction(ApplyInput) => ("apply_code_action", "Apply a listed text-edit code action. Commands and resource operations are unsupported."),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidInput,
    OperationFailed,
    Unavailable,
    PermissionDenied,
    ConnectionRefused,
    TimedOut,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}
impl ApiError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidInput,
            message: message.into(),
        }
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Unavailable,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ApiError {}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApiResponse {
    Success { data: Value },
    Failure { error: ApiError },
}

pub struct Application {
    core: Arc<Core>,
    actions: Mutex<HashMap<String, CachedAction>>,
    plans: Mutex<HashMap<String, RenamePlan>>,
}
impl Application {
    pub fn new(core: Arc<Core>) -> Self {
        Self {
            core,
            actions: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
        }
    }
    pub async fn execute(&self, request: Request) -> Result<Value, ApiError> {
        request.validate()?;
        self.dispatch(request).await.map_err(|error| ApiError {
            code: ErrorCode::OperationFailed,
            message: format!("{error:#}"),
        })
    }
    async fn dispatch(&self, request: Request) -> Result<Value> {
        match request {
            Request::Backend(input) => Ok(output::status(
                self.core
                    .set_typescript_backend(&input.workspace, input.backend)
                    .await?,
                input.verbose.unwrap_or(false),
            )),
            Request::Connect(input) => Ok(output::status(
                self.core.connect(&input.workspace).await?.status().await,
                input.verbose.unwrap_or(false),
            )),
            Request::Disconnect(input) => {
                let result = self.core.disconnect(&input.workspace).await?;
                let root = result["workspace"].as_str().unwrap();
                self.actions
                    .lock()
                    .await
                    .retain(|_, action| action.root != root);
                self.plans.lock().await.retain(|_, plan| plan.root != root);
                Ok(result)
            }
            Request::Status(input) => match input.workspace {
                Some(root) => Ok(output::status(
                    self.core.workspace(&root).await?.status().await,
                    input.verbose.unwrap_or(false),
                )),
                None => Ok(output::status(
                    self.core.status().await,
                    input.verbose.unwrap_or(false),
                )),
            },
            Request::Diagnostics(input) => Ok(output::diagnostics(
                self.core
                    .workspace(&input.workspace)
                    .await?
                    .diagnostics(input.check.unwrap_or(false))
                    .await?,
                input.verbose.unwrap_or(false),
            )),
            Request::WorkspaceSymbols(input) => {
                let workspace = self.core.workspace(&input.workspace).await?;
                let result = workspace.workspace_symbols(&input.query).await?;
                if input.verbose.unwrap_or(false) {
                    return Ok(result);
                }
                let mut items = symbols::flatten(&result, "");
                for item in &mut items {
                    let path = workspace.path(item["uri"].as_str().unwrap_or(""));
                    item["path"] = match path {
                        Ok(path) => json!(path.strip_prefix(&workspace.root).unwrap_or(&path)),
                        Err(_) => item["uri"].clone(),
                    };
                }
                Ok(symbols::compact(items))
            }
            Request::DocumentSymbols(input) => {
                let workspace = self.core.workspace(&input.workspace).await?;
                let uri = workspace.open(&input.path).await?;
                let result = workspace
                    .language_server(&uri)
                    .await?
                    .request(
                        "textDocument/documentSymbol",
                        json!({"textDocument":{"uri":uri}}),
                    )
                    .await?;
                if input.verbose.unwrap_or(false) {
                    Ok(result)
                } else {
                    Ok(symbols::compact(symbols::flatten(&result, &uri)))
                }
            }

            Request::Definition(input) => {
                self.at_position(input, "textDocument/definition", false)
                    .await
            }
            Request::Hover(input) => self.at_position(input, "textDocument/hover", false).await,
            Request::References(input) => {
                self.at_position(input, "textDocument/references", true)
                    .await
            }
            Request::Rename(input) => self.rename(input).await,
            Request::ApplyRename(input) => self.apply_rename(input).await,
            Request::CodeActions(input) => self.code_actions(input).await,
            Request::ApplyCodeAction(input) => self.apply_action(input).await,
        }
    }
    async fn at_position(
        &self,
        input: PositionInput,
        method: &str,
        references: bool,
    ) -> Result<Value> {
        let workspace = self.core.workspace(&input.workspace).await?;
        let uri = workspace.open(&input.path).await?;
        let _edit = workspace.edit_lock.lock().await;
        let selection = select_position(
            &workspace,
            &uri,
            input.line,
            input.character,
            input.symbol.as_deref(),
        )
        .await?;
        let position = match selection {
            Selection::Position(position) => position,
            Selection::Candidates(value) => return Ok(value),
        };
        let mut params = json!({"textDocument":{"uri":uri},"position":position});
        if references {
            params["context"] = json!({"includeDeclaration":true});
        }
        workspace
            .language_server(&uri)
            .await?
            .request(method, params)
            .await
    }
    async fn code_actions(&self, input: ActionsInput) -> Result<Value> {
        let workspace = self.core.workspace(&input.workspace).await?;
        let uri = workspace.open(&input.path).await?;
        let _edit = workspace.edit_lock.lock().await;
        let snapshot = Arc::new(workspace.snapshot().await?);
        let position = Position {
            line: input.line,
            character: input.character,
        };
        let end = input.end.as_ref().unwrap_or(&position);
        if (end.line, end.character) < (position.line, position.character) {
            bail!("Range end precedes start");
        }
        let server = workspace.language_server(&uri).await?;
        let diagnostics = server
            .state
            .read()
            .await
            .diagnostics
            .get(&uri)
            .map(|p| p["diagnostics"].clone())
            .unwrap_or(json!([]));
        let actions = workspace.language_server(&uri).await?.request("textDocument/codeAction", json!({"textDocument":{"uri":uri},"range":{"start":position,"end":end},"context":{"diagnostics":diagnostics}})).await?;
        workspace.verify_snapshot(&snapshot).await?;
        let mut results = vec![];
        let mut cache = self.actions.lock().await;
        cache.retain(|_, action| action.created.elapsed() < PLAN_TTL);
        if cache.len() + actions.as_array().map_or(0, Vec::len) > 128 {
            bail!("Code action cache full; retry after cached actions expire");
        }
        for action in actions.as_array().unwrap_or(&vec![]) {
            let id = uuid::Uuid::new_v4().to_string();
            cache.insert(
                id.clone(),
                CachedAction {
                    root: workspace.root.to_string_lossy().into(),
                    uri: uri.clone(),
                    action: action.clone(),
                    snapshot: snapshot.clone(),
                    created: Instant::now(),
                },
            );
            results.push(json!({"action_id":id,"title":action["title"],"kind":action["kind"],"disabled":action["disabled"],"requiresCommand":!action["command"].is_null()}));
        }
        Ok(json!(results))
    }
    async fn apply_action(&self, input: ApplyInput) -> Result<Value> {
        let workspace: Arc<Workspace> = self.core.workspace(&input.workspace).await?;
        let cached = self
            .actions
            .lock()
            .await
            .get(&input.action_id)
            .cloned()
            .context("Unknown action_id; call code_actions first")?;
        if cached.root != workspace.root.to_string_lossy() {
            bail!("Action belongs to another workspace");
        }
        if cached.created.elapsed() >= PLAN_TTL {
            self.actions.lock().await.remove(&input.action_id);
            bail!("Code action expired; request it again");
        }
        let _edit = workspace.edit_lock.lock().await;
        workspace.verify_snapshot(&cached.snapshot).await?;
        let uri = cached.uri;
        let mut action = cached.action;
        if !action["disabled"].is_null() {
            bail!("Code action is disabled: {}", action["disabled"]);
        }
        if action.get("edit").is_none() && action.get("data").is_some() {
            action = workspace
                .language_server(&uri)
                .await?
                .request("codeAction/resolve", action)
                .await?;
        }
        if !action["command"].is_null() {
            bail!("Command-based code actions are not supported; no edits applied");
        }
        let edit = workspace
            .prepare_edit(
                action.get("edit").context("Action has no text edit")?,
                &cached.snapshot,
            )
            .await?;
        let result = workspace
            .commit_edit(&edit, &cached.snapshot, input.verbose.unwrap_or(false))
            .await?;
        self.actions.lock().await.remove(&input.action_id);
        Ok(result)
    }
}

const PLAN_TTL: Duration = Duration::from_secs(300);
#[derive(Clone)]
struct CachedAction {
    root: String,
    uri: String,
    action: Value,
    snapshot: Arc<Snapshot>,
    created: Instant,
}
struct RenamePlan {
    root: String,
    snapshot: Snapshot,
    edit: PreparedEdit,
    new_name: String,
    symbol: Option<String>,
    created: Instant,
}

fn validate_selection(
    line: Option<u32>,
    character: Option<u32>,
    symbol: Option<&str>,
) -> Result<(), ApiError> {
    match (line, character, symbol) {
        (Some(_), Some(_), None) => Ok(()),
        (None, None, Some(name)) if !name.trim().is_empty() => Ok(()),
        _ => Err(ApiError::invalid(
            "Supply either symbol or both line and character (zero-based UTF-16), not both",
        )),
    }
}
impl Request {
    fn validate(&self) -> Result<(), ApiError> {
        match self {
            Self::Rename(input) => {
                validate_selection(input.line, input.character, input.symbol.as_deref())?;
                if input.new_name.trim().is_empty() {
                    return Err(ApiError::invalid("new_name must not be empty"));
                }
                if input.preview == Some(true) && input.apply == Some(true) {
                    return Err(ApiError::invalid("preview and apply cannot both be true"));
                }
            }
            Self::Definition(input) | Self::References(input) | Self::Hover(input) => {
                validate_selection(input.line, input.character, input.symbol.as_deref())?
            }
            _ => {}
        }
        Ok(())
    }
}
enum Selection {
    Position(Value),
    Candidates(Value),
}
async fn select_position(
    workspace: &Workspace,
    uri: &str,
    line: Option<u32>,
    character: Option<u32>,
    symbol: Option<&str>,
) -> Result<Selection> {
    if let (Some(line), Some(character)) = (line, character) {
        return Ok(Selection::Position(
            json!({"line":line,"character":character}),
        ));
    }
    let name = symbol.context("Missing symbol")?;
    let server = workspace.language_server(uri).await?;
    let result = server
        .request(
            "textDocument/documentSymbol",
            json!({"textDocument":{"uri":uri}}),
        )
        .await?;
    let matches: Vec<_> = symbols::flatten(&result, uri)
        .into_iter()
        .filter(|s| s["name"] == name)
        .collect();
    if matches.len() != 1 {
        return Ok(Selection::Candidates(
            json!({"applied":false,"reason":if matches.is_empty(){"symbol_not_found"}else{"ambiguous_symbol"},"symbol":name,"candidates":symbols::compact(matches)}),
        ));
    }
    let item = &matches[0];
    // Flat SymbolInformation may point at the whole declaration, not the identifier.
    // Verify its returned start against source; never text-search and guess a declaration.
    if item["exactSelection"] != true {
        let text = workspace.document_text(uri).await?;
        let position = json!({"line":item["line"],"character":item["character"]});
        let start = crate::protocol::offset(&text, &position)?;
        if !text[start..].starts_with(name) {
            bail!(
                "Server did not provide an exact symbol selection; use explicit line and character"
            );
        }
    }
    Ok(Selection::Position(
        json!({"line":item["line"],"character":item["character"]}),
    ))
}
impl Application {
    async fn rename(&self, input: RenameInput) -> Result<Value> {
        let workspace = self.core.workspace(&input.workspace).await?;
        let uri = workspace.open(&input.path).await?;
        let _edit = workspace.edit_lock.lock().await;
        let snapshot = workspace.snapshot().await?;
        let position = match select_position(
            &workspace,
            &uri,
            input.line,
            input.character,
            input.symbol.as_deref(),
        )
        .await?
        {
            Selection::Position(position) => position,
            Selection::Candidates(value) => return Ok(value),
        };
        let server = workspace.language_server(&uri).await?;
        let mut symbol = input.symbol;
        if server.capabilities.read().await["renameProvider"]["prepareProvider"] == true {
            let prepared = server
                .request(
                    "textDocument/prepareRename",
                    json!({"textDocument":{"uri":uri},"position":position}),
                )
                .await?;
            if prepared.is_null() {
                bail!("No rename available at this position");
            }
            let range = prepared.get("range").unwrap_or(&prepared);
            if range.get("start").is_some() {
                let text = workspace.document_text(&uri).await?;
                let start = crate::protocol::offset(&text, &range["start"])?;
                let end = crate::protocol::offset(&text, &range["end"])?;
                symbol = Some(
                    text.get(start..end)
                        .context("Invalid prepareRename range")?
                        .into(),
                );
            }
        }
        let edit = server
            .request(
                "textDocument/rename",
                json!({"textDocument":{"uri":uri},"position":position,"newName":input.new_name}),
            )
            .await?;
        if edit.is_null() {
            bail!("No rename available at this position");
        }
        let prepared = workspace.prepare_edit(&edit, &snapshot).await?;
        let preview = input.preview.unwrap_or(false) || input.apply == Some(false);
        let verbose = input.verbose.unwrap_or(false);
        let mut result = if preview {
            let mut cache = self.plans.lock().await;
            cache.retain(|_, plan| plan.created.elapsed() < PLAN_TTL);
            if cache.len() >= 8 {
                bail!("Preview cache full (8 plans); apply an existing plan or wait for expiry");
            }
            let id = uuid::Uuid::new_v4().to_string();
            let mut result = prepared.receipt(&workspace.root, false, verbose);
            result["planId"] = json!(id);
            result["expiresInSeconds"] = json!(PLAN_TTL.as_secs());
            cache.insert(
                id,
                RenamePlan {
                    root: workspace.root.to_string_lossy().into(),
                    snapshot,
                    edit: prepared,
                    new_name: input.new_name.clone(),
                    symbol: symbol.clone(),
                    created: Instant::now(),
                },
            );
            result
        } else {
            workspace.commit_edit(&prepared, &snapshot, verbose).await?
        };
        result["newName"] = json!(input.new_name);
        if let Some(symbol) = symbol {
            result["symbol"] = json!(symbol);
        }
        Ok(result)
    }
    async fn apply_rename(&self, input: PlanInput) -> Result<Value> {
        let workspace = self.core.workspace(&input.workspace).await?;
        // Take once, so two concurrent callers cannot both apply the same plan.
        let mut cache = self.plans.lock().await;
        let plan = cache
            .get(&input.plan_id)
            .context("Unknown plan_id; request a new preview")?;
        if plan.root != workspace.root.to_string_lossy() {
            bail!("Plan belongs to another workspace");
        }
        let plan = cache.remove(&input.plan_id).unwrap();
        drop(cache);
        if plan.created.elapsed() >= PLAN_TTL {
            bail!("Preview expired; request a new preview");
        }
        let _edit = workspace.edit_lock.lock().await;
        let mut result = workspace
            .commit_edit(&plan.edit, &plan.snapshot, input.verbose.unwrap_or(false))
            .await?;
        result["planId"] = json!(input.plan_id);
        result["newName"] = json!(plan.new_name);
        if let Some(symbol) = plan.symbol {
            result["symbol"] = json!(symbol);
        }
        Ok(result)
    }
}
