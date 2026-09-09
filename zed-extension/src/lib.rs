use zed_extension_api::{
    self as zed, settings::LspSettings, Command, LanguageServerId, Result, Worktree,
};
struct Bridge;
impl zed::Extension for Bridge {
    fn new() -> Self {
        Self
    }
    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let settings = LspSettings::for_worktree(id.as_ref(), worktree)?;
        let binary = settings.binary;
        let command = binary
            .as_ref()
            .and_then(|b| b.path.clone())
            .or_else(|| worktree.which("mcp-editor-lsp-bridge"))
            .ok_or(
                "Install mcp-editor-lsp-bridge or configure lsp.editor-lsp-bridge.binary.path",
            )?;
        let args = binary
            .and_then(|b| b.arguments)
            .unwrap_or_else(|| vec!["companion".into()]);
        Ok(Command {
            command,
            args,
            env: worktree.shell_env(),
        })
    }
}
zed::register_extension!(Bridge);
