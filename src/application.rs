//! Transport-independent inbound port and semantic application service.
use crate::core::{Core, Workspace};
use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
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
input!(StatusInput { workspace: Option<String> });
input!(WorkspaceInput {
    workspace: String,
    query: String
});
input!(DocumentInput {
    workspace: String,
    path: String
});
input!(PositionInput {
    workspace: String,
    path: String,
    line: u32,
    character: u32
});
input!(DiagnosticsInput { workspace: String, check: Option<bool> });
input!(RenameInput { workspace: String, path: String, line: u32, character: u32, new_name: String, apply: Option<bool> });
input!(ActionsInput { workspace: String, path: String, line: u32, character: u32, end: Option<Position> });
input!(ApplyInput {
    workspace: String,
    action_id: String
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
                serde_json::from_value(json!({"operation":name,"arguments":arguments}))
                    .map_err(|error| ApiError::invalid(error.to_string()))
            }
        }
        pub fn operations() -> Vec<OperationInfo> {
            vec![$(OperationInfo { name: $name, description: $description, input_schema: serde_json::to_value(schema_for!($input)).expect("serializable schema") }),*]
        }
    };
}
operations! {
    Status(StatusInput) => ("workspace_status", "List sessions, or attach a Cargo workspace and show analyzer, companion and check state."),
    WorkspaceSymbols(WorkspaceInput) => ("workspace_symbols", "Find Rust symbols by query in the workspace."),
    DocumentSymbols(DocumentInput) => ("document_symbols", "List symbols in a Rust file."),
    Definition(PositionInput) => ("definition", "Find a Rust symbol definition. Positions are zero-based UTF-16."),
    References(PositionInput) => ("references", "Find Rust symbol references including declaration. Positions are zero-based UTF-16."),
    Hover(PositionInput) => ("hover", "Get Rust type and documentation. Positions are zero-based UTF-16."),
    Diagnostics(DiagnosticsInput) => ("diagnostics", "Get live diagnostics and saved-file check state. check=true awaits a shared Cargo check. Empty live diagnostics alone never mean clean."),
    Rename(RenameInput) => ("rename", "Preview a semantic rename; apply=true writes to disk with precedence over unsaved editor contents."),
    CodeActions(ActionsInput) => ("code_actions", "List code actions at a zero-based UTF-16 position/range. Use action_id with apply_code_action."),
    ApplyCodeAction(ApplyInput) => ("apply_code_action", "Apply a listed text-edit code action. Commands and resource operations are unsupported."),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidInput,
    OperationFailed,
    Unavailable,
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
    actions: Mutex<HashMap<String, (String, Value)>>,
}
impl Application {
    pub fn new(core: Arc<Core>) -> Self {
        Self {
            core,
            actions: Mutex::new(HashMap::new()),
        }
    }
    pub async fn execute(&self, request: Request) -> Result<Value, ApiError> {
        self.dispatch(request).await.map_err(|error| ApiError {
            code: ErrorCode::OperationFailed,
            message: format!("{error:#}"),
        })
    }
    async fn dispatch(&self, request: Request) -> Result<Value> {
        match request {
            Request::Status(input) => match input.workspace {
                Some(root) => Ok(self.core.workspace(&root).await?.status().await),
                None => Ok(self.core.status().await),
            },
            Request::Diagnostics(input) => {
                self.core
                    .workspace(&input.workspace)
                    .await?
                    .diagnostics(input.check.unwrap_or(false))
                    .await
            }
            Request::WorkspaceSymbols(input) => {
                self.core
                    .workspace(&input.workspace)
                    .await?
                    .lsp
                    .request("workspace/symbol", json!({"query":input.query}))
                    .await
            }
            Request::DocumentSymbols(input) => {
                let workspace = self.core.workspace(&input.workspace).await?;
                let uri = workspace.open(&input.path).await?;
                workspace
                    .lsp
                    .request(
                        "textDocument/documentSymbol",
                        json!({"textDocument":{"uri":uri}}),
                    )
                    .await
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
            Request::Rename(input) => {
                let workspace = self.core.workspace(&input.workspace).await?;
                let uri = workspace.open(&input.path).await?;
                let edit = workspace.lsp.request("textDocument/rename", json!({"textDocument":{"uri":uri},"position":{"line":input.line,"character":input.character},"newName":input.new_name})).await?;
                if edit.is_null() {
                    bail!("No rename available at this position");
                }
                if input.apply.unwrap_or(false) {
                    workspace.apply_edit(&edit).await
                } else {
                    Ok(edit)
                }
            }
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
        let mut params = json!({"textDocument":{"uri":uri},"position":{"line":input.line,"character":input.character}});
        if references {
            params["context"] = json!({"includeDeclaration":true});
        }
        workspace.lsp.request(method, params).await
    }
    async fn code_actions(&self, input: ActionsInput) -> Result<Value> {
        let workspace = self.core.workspace(&input.workspace).await?;
        let uri = workspace.open(&input.path).await?;
        let position = Position {
            line: input.line,
            character: input.character,
        };
        let end = input.end.as_ref().unwrap_or(&position);
        if (end.line, end.character) < (position.line, position.character) {
            bail!("Range end precedes start");
        }
        let diagnostics = workspace
            .lsp
            .state
            .read()
            .await
            .diagnostics
            .get(&uri)
            .map(|p| p["diagnostics"].clone())
            .unwrap_or(json!([]));
        let actions = workspace.lsp.request("textDocument/codeAction", json!({"textDocument":{"uri":uri},"range":{"start":position,"end":end},"context":{"diagnostics":diagnostics}})).await?;
        let mut results = vec![];
        let mut cache = self.actions.lock().await;
        for action in actions.as_array().unwrap_or(&vec![]) {
            let id = uuid::Uuid::new_v4().to_string();
            cache.insert(
                id.clone(),
                (workspace.root.to_string_lossy().into(), action.clone()),
            );
            results.push(json!({"action_id":id,"title":action["title"],"kind":action["kind"],"disabled":action["disabled"],"requiresCommand":action.get("command").is_some()}));
        }
        Ok(json!(results))
    }
    async fn apply_action(&self, input: ApplyInput) -> Result<Value> {
        let workspace: Arc<Workspace> = self.core.workspace(&input.workspace).await?;
        let (root, mut action) = self
            .actions
            .lock()
            .await
            .get(&input.action_id)
            .cloned()
            .context("Unknown action_id; call code_actions first")?;
        if root != workspace.root.to_string_lossy() {
            bail!("Action belongs to another workspace");
        }
        if action.get("disabled").is_some() {
            bail!("Code action is disabled: {}", action["disabled"]);
        }
        if action.get("edit").is_none() && action.get("data").is_some() {
            action = workspace.lsp.request("codeAction/resolve", action).await?;
        }
        if action.get("command").is_some() {
            bail!("Command-based code actions are not supported; no edits applied");
        }
        let result = workspace
            .apply_edit(action.get("edit").context("Action has no text edit")?)
            .await?;
        self.actions.lock().await.remove(&input.action_id);
        Ok(result)
    }
}
