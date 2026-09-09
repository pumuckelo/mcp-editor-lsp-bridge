use crate::application::{self, ApiError, Application, Request};
use rmcp::{ErrorData, RoleServer, ServerHandler, model::*, service::RequestContext};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub struct Mcp {
    application: Arc<Application>,
}
impl Mcp {
    pub fn new(application: Arc<Application>) -> Self {
        Self { application }
    }
    async fn call(&self, name: &str, arguments: Value) -> Result<Value, ApiError> {
        self.application
            .execute(Request::decode(name, arguments)?)
            .await
    }
}
fn tools() -> Vec<Tool> {
    application::operations()
        .into_iter()
        .map(|operation| {
            Tool::new(
                operation.name,
                operation.description,
                operation.input_schema.as_object().unwrap().clone(),
            )
        })
        .collect()
}
impl ServerHandler for Mcp {
    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|tool| tool.name == name)
    }
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some("Rust semantic tools. Use absolute workspace directories, zero-based lines and UTF-16 characters. Call workspace_status to inspect readiness. Agent edits to disk take precedence over unsaved companion snapshots.".into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let result = match self
            .call(&request.name, json!(request.arguments.unwrap_or_default()))
            .await
        {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&value).unwrap(),
            )]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(
                serde_json::to_string(&json!({"error":error})).unwrap(),
            )]),
        };
        Ok(result.into())
    }
}
