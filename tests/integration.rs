use anyhow::{Context, Result, bail};
use mcp_editor_lsp_bridge::{
    application::{Application, Request},
    core::{Config, Core, SyncEvent},
    server,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

async fn wait_ready(workspace: &mcp_editor_lsp_bridge::core::Workspace) -> Result<()> {
    for _ in 0..300 {
        if workspace.status().await["ready"] == true {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "Analyzer did not become ready: {}",
        workspace.status().await
    )
}
fn event(root: &str, kind: &str, uri: Option<&str>, text: Option<&str>, epoch: u64) -> SyncEvent {
    SyncEvent {
        workspace: root.into(),
        companion: "test-zed".into(),
        kind: kind.into(),
        uri: uri.map(Into::into),
        text: text.map(Into::into),
        epoch,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires rust-analyzer and Cargo; exercises real subprocesses"]
async fn real_workspace_mcp_and_companion() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    std::fs::create_dir(root.join("src"))?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"bridge_fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )?;
    std::fs::write(
        root.join("src/lib.rs"),
        "mod helper;\npub fn run() -> i32 { helper::answer() }\n",
    )?;
    std::fs::write(
        root.join("src/helper.rs"),
        "pub fn answer() -> i32 { 42 }\n",
    )?;
    let root_str = root.to_str().unwrap();
    let core = Core::new(Config::default());
    let workspace = core.workspace(root_str).await?;
    let same = core.workspace(root_str).await?;
    assert!(Arc::ptr_eq(&workspace, &same));
    wait_ready(&workspace).await?;
    let tools = TestApplication(Application::new(core.clone()));
    let args = json!({"workspace":root_str,"path":"src/lib.rs","line":1,"character":33});
    let definition = tools.execute("definition", args.clone()).await?;
    assert!(definition.to_string().contains("helper.rs"), "{definition}");
    let uri = workspace.open("src/helper.rs").await?;
    workspace
        .sync(event(root_str, "connect", None, None, 0))
        .await?;
    assert_eq!(
        workspace
            .sync(event(
                root_str,
                "open",
                Some(&uri),
                Some("pub fn answer() -> &'static str { \"dirty\" }\n"),
                0
            ))
            .await?["accepted"],
        true
    );
    let hover = tools
        .execute(
            "hover",
            json!({"workspace":root_str,"path":"src/helper.rs","line":0,"character":8}),
        )
        .await?;
    assert!(hover.to_string().contains("str"), "{hover}");
    std::fs::write(
        root.join("src/helper.rs"),
        "pub fn answer() -> i32 { 77 }\n",
    )?;
    workspace.disk_changed(&root.join("src/helper.rs")).await?;
    let rejected = workspace
        .sync(event(
            root_str,
            "change",
            Some(&uri),
            Some("pub fn answer() -> bool { true }\n"),
            0,
        ))
        .await?;
    assert_eq!(rejected["accepted"], false);
    let hover = tools
        .execute(
            "hover",
            json!({"workspace":root_str,"path":"src/helper.rs","line":0,"character":8}),
        )
        .await?;
    assert!(hover.to_string().contains("i32"), "{hover}");
    let rename=tools.execute("rename",json!({"workspace":root_str,"path":"src/helper.rs","line":0,"character":8,"new_name":"result","apply":true})).await?;
    assert_eq!(rename["applied"], true);
    assert!(std::fs::read_to_string(root.join("src/lib.rs"))?.contains("helper::result()"));
    assert!(std::fs::read_to_string(root.join("src/helper.rs"))?.contains("fn result()"));
    workspace
        .sync(event(root_str, "disconnect", None, None, 0))
        .await?;
    // File watcher updates an already-open document without an explicit bridge mutation.
    std::fs::write(
        root.join("src/helper.rs"),
        "pub fn result() -> i64 { 77 }\n",
    )?;
    for _ in 0..50 {
        let result = workspace
            .lsp
            .request(
                "textDocument/hover",
                json!({"textDocument":{"uri":uri},"position":{"line":0,"character":8}}),
            )
            .await?;
        if result.to_string().contains("i64") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let hover = workspace
        .lsp
        .request(
            "textDocument/hover",
            json!({"textDocument":{"uri":uri},"position":{"line":0,"character":8}}),
        )
        .await?;
    assert!(hover.to_string().contains("i64"));
    let diagnostics = workspace.diagnostics(true, None).await?;
    assert_eq!(
        diagnostics["savedFileCheck"]["success"], false,
        "{diagnostics}"
    );
    assert!(
        !diagnostics["savedFileCheck"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    std::fs::write(
        root.join("src/helper.rs"),
        "pub fn result() -> i32 { 77 }\n",
    )?;
    workspace.disk_changed(&root.join("src/helper.rs")).await?;
    let diagnostics = workspace.diagnostics(true, None).await?;
    assert_eq!(
        diagnostics["savedFileCheck"]["success"], true,
        "{diagnostics}"
    );
    let actions = tools
        .execute(
            "code_actions",
            json!({"workspace":root_str,"path":"src/helper.rs","line":0,"character":25}),
        )
        .await?;
    let action = actions
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["title"].as_str().unwrap_or("").contains("0x4D"))
        .with_context(|| format!("No hexadecimal assist: {actions}"))?;
    let applied = tools
        .execute(
            "apply_code_action",
            json!({"workspace":root_str,"action_id":action["action_id"]}),
        )
        .await?;
    assert_eq!(applied["applied"], true);
    assert!(std::fs::read_to_string(root.join("src/helper.rs"))?.contains("0x4D"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let app = server::router(core.clone(), port);
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoint = format!("http://127.0.0.1:{port}/mcp");
    for _ in 0..2 {
        let client = reqwest::Client::new();
        for message in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"integration","version":"1"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"workspace_status","arguments":{"workspace":root_str}}}),
        ] {
            let response = client
                .post(&endpoint)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-03-26")
                .json(&message)
                .send()
                .await?;
            let status = response.status();
            let body = response.text().await?;
            assert!(status.is_success(), "{status}: {body}");
            let result: Value = serde_json::from_str(&body)?;
            assert!(result.get("error").is_none(), "{result}");
        }
    }
    // CLI and MCP share the same analyzer and application action cache.
    let base = format!("http://127.0.0.1:{port}");
    let cli_status = cli_call(&root.join("src"), &base, &["workspace-status"]).await?;
    assert_eq!(
        cli_status["analyzerPid"],
        workspace.status().await["analyzerPid"]
    );
    let cli_definition = cli_call(
        &root.join("src"),
        &base,
        &[
            "definition",
            "--path",
            "lib.rs",
            "--line",
            "1",
            "--character",
            "33",
        ],
    )
    .await?;
    assert_eq!(
        cli_definition,
        tools.execute("definition", args.clone()).await?
    );
    let preview = cli_call(
        &root,
        &base,
        &[
            "rename",
            "--json",
            &json!({"path":"src/helper.rs","line":0,"character":8,"new_name":"preview_only","preview":true})
                .to_string(),
        ],
    )
    .await?;
    assert!(preview.to_string().contains("preview_only"));
    assert!(std::fs::read_to_string(root.join("src/helper.rs"))?.contains("result"));
    let checked = cli_call(&root, &base, &["diagnostics", "--check"]).await?;
    assert_eq!(checked["savedFileCheck"]["success"], true);
    // Exercise the actual CLI mutation path, then reverse it through CLI too.
    let before_helper = std::fs::read_to_string(root.join("src/helper.rs"))?;
    let before_lib = std::fs::read_to_string(root.join("src/lib.rs"))?;
    let renamed = cli_call(
        &root,
        &base,
        &[
            "rename",
            "--path",
            "src/helper.rs",
            "--line",
            "0",
            "--character",
            "8",
            "--new-name",
            "cli_renamed_result",
            "--apply",
        ],
    )
    .await?;
    assert_eq!(renamed["applied"], true, "{renamed}");
    assert_eq!(
        std::fs::read_to_string(root.join("src/helper.rs"))?,
        before_helper.replace("result", "cli_renamed_result")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs"))?,
        before_lib.replace("result", "cli_renamed_result")
    );
    let references = cli_call(
        &root,
        &base,
        &[
            "references",
            "--path",
            "src/helper.rs",
            "--line",
            "0",
            "--character",
            "8",
        ],
    )
    .await?;
    assert!(references.to_string().contains("helper.rs"), "{references}");
    assert!(references.to_string().contains("lib.rs"), "{references}");
    let checked = cli_call(&root, &base, &["diagnostics", "--check"]).await?;
    assert_eq!(checked["savedFileCheck"]["success"], true, "{checked}");
    let restored = cli_call(&root, &base, &[
        "rename", "--json", &json!({"path":"src/helper.rs","line":0,"character":8,"new_name":"result","apply":true}).to_string(),
    ]).await?;
    assert_eq!(restored["applied"], true, "{restored}");
    assert_eq!(
        std::fs::read_to_string(root.join("src/helper.rs"))?,
        before_helper
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs"))?,
        before_lib
    );
    // Rust symbol selection and a preview produced by CLI then applied through MCP.
    let planned = cli_call(
        &root,
        &base,
        &[
            "rename",
            "--path",
            "src/helper.rs",
            "--symbol",
            "result",
            "--new-name",
            "planned_result",
            "--preview",
        ],
    )
    .await?;
    assert_eq!(planned["applied"], false);
    assert_eq!(planned["fileCount"], 2);
    assert!(std::fs::read_to_string(root.join("src/helper.rs"))?.contains("fn result"));
    let applied = mcp_call(
        &endpoint,
        "apply_rename",
        json!({"workspace":root_str,"plan_id":planned["planId"]}),
    )
    .await?;
    assert_eq!(applied["applied"], true);
    assert!(std::fs::read_to_string(root.join("src/lib.rs"))?.contains("planned_result"));
    let restored = cli_call(
        &root,
        &base,
        &[
            "rename",
            "--path",
            "src/helper.rs",
            "--symbol",
            "planned_result",
            "--new-name",
            "result",
        ],
    )
    .await?;
    assert_eq!(restored["applied"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("src/helper.rs"))?,
        before_helper
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs"))?,
        before_lib
    );
    for from_mcp in [true, false] {
        std::fs::write(
            root.join("src/helper.rs"),
            "pub fn result() -> i32 { 77 }\n",
        )?;
        workspace.disk_changed(&root.join("src/helper.rs")).await?;
        let input = json!({"workspace":root_str,"path":"src/helper.rs","line":0,"character":25});
        let actions = if from_mcp {
            mcp_call(&endpoint, "code_actions", input).await?
        } else {
            cli_call(
                &root,
                &base,
                &["code-actions", "--json", &input.to_string()],
            )
            .await?
        };
        let action = actions
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["title"].as_str().unwrap_or("").contains("0x4D"))
            .context("Missing hex assist")?;
        let applied = if from_mcp {
            cli_call(
                &root,
                &base,
                &[
                    "apply-code-action",
                    "--action-id",
                    action["action_id"].as_str().unwrap(),
                ],
            )
            .await?
        } else {
            mcp_call(
                &endpoint,
                "apply_code_action",
                json!({"workspace":root_str,"action_id":action["action_id"]}),
            )
            .await?
        };
        assert_eq!(applied["applied"], true);
        assert!(std::fs::read_to_string(root.join("src/helper.rs"))?.contains("0x4D"));
    }
    let invalid = reqwest::Client::new().post(format!("{base}/api/execute")).json(&json!({"operation":"definition","arguments":{"workspace":root_str,"path":"src/lib.rs","line":-1,"character":0}})).send().await?;
    assert_eq!(invalid.status(), 400);
    assert_eq!(
        invalid.json::<Value>().await?["error"]["code"],
        "INVALID_INPUT"
    );
    assert_eq!(core.sessions.lock().await.len(), 1);
    // Cross-origin browser requests must not be able to mutate a local workspace.
    let forbidden = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/workspaces"))
        .header("origin", "https://example.com")
        .json(&json!({"workspace":root_str}))
        .send()
        .await?;
    assert_eq!(forbidden.status(), 403);
    // Exercise the real companion executable, including a disconnect/reconnect.
    use mcp_editor_lsp_bridge::protocol::{read_message, write_message};
    use tokio::{io::BufReader, process::Command};
    let mut companion = Command::new(env!("CARGO_BIN_EXE_mcp-editor-lsp-bridge"))
        .args([
            "companion",
            "--endpoint",
            &format!("http://127.0.0.1:{port}"),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut writer = companion.stdin.take().unwrap();
    let mut reader = BufReader::new(companion.stdout.take().unwrap());
    write_message(&mut writer,&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":url::Url::from_directory_path(&root).unwrap().as_str()}})).await?;
    let initialized = read_message(&mut reader).await?.unwrap();
    assert_eq!(
        initialized["result"]["capabilities"]["textDocumentSync"]["change"],
        1
    );
    let lib_uri = workspace.uri(&root.join("src/lib.rs"))?;
    write_message(&mut writer,&json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":lib_uri,"version":1,"text":"pub fn fresh_overlay() {}"}}})).await?;
    // First snapshot may be superseded by earlier disk mutations in this session.
    tokio::time::sleep(Duration::from_secs(4)).await;
    write_message(&mut writer,&json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":lib_uri,"version":2},"contentChanges":[{"text":"pub fn fresh_overlay_2() {}"}]}})).await?;
    for _ in 0..50 {
        let symbols = tools
            .execute(
                "document_symbols",
                json!({"workspace":root_str,"path":"src/lib.rs"}),
            )
            .await?;
        if symbols.to_string().contains("fresh_overlay_2") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let symbols = tools
        .execute(
            "document_symbols",
            json!({"workspace":root_str,"path":"src/lib.rs"}),
        )
        .await?;
    assert!(symbols.to_string().contains("fresh_overlay_2"), "{symbols}");
    // Reject a stale editor snapshot, then prove reconnect does not replay it.
    std::fs::write(root.join("src/lib.rs"), "pub fn agent_wins() {}\n")?;
    workspace.disk_changed(&root.join("src/lib.rs")).await?;
    write_message(&mut writer, &json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":lib_uri,"version":3},"contentChanges":[{"text":"pub fn stale_editor() {}"}]}})).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let symbols = tools
        .execute(
            "document_symbols",
            json!({"workspace":root_str,"path":"src/lib.rs"}),
        )
        .await?;
    assert!(symbols.to_string().contains("agent_wins"), "{symbols}");
    let connected_id = workspace.status().await["companion"]
        .as_str()
        .unwrap()
        .to_owned();
    workspace
        .sync(SyncEvent {
            workspace: root_str.into(),
            companion: connected_id,
            kind: "disconnect".into(),
            uri: None,
            text: None,
            epoch: 0,
        })
        .await?;
    for _ in 0..100 {
        if workspace.status().await["companion"].is_string() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(workspace.status().await["companion"].is_string());
    let symbols = tools
        .execute(
            "document_symbols",
            json!({"workspace":root_str,"path":"src/lib.rs"}),
        )
        .await?;
    assert!(
        symbols.to_string().contains("agent_wins"),
        "Reconnect replayed a superseded editor buffer: {symbols}"
    );
    write_message(&mut writer, &json!({"jsonrpc":"2.0","method":"exit"})).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), companion.wait())
            .await??
            .success()
    );
    assert!(workspace.status().await["companion"].is_null());
    let symbols = tools
        .execute(
            "document_symbols",
            json!({"workspace":root_str,"path":"src/lib.rs"}),
        )
        .await?;
    assert!(symbols.to_string().contains("agent_wins"));
    // A distinct checkout directory creates a distinct analyzer, even for the same package.
    let second = tempfile::tempdir()?;
    std::fs::create_dir(second.path().join("src"))?;
    std::fs::copy(root.join("Cargo.toml"), second.path().join("Cargo.toml"))?;
    std::fs::write(second.path().join("src/lib.rs"), "pub fn separate() {}")?;
    let second_workspace = core.workspace(second.path().to_str().unwrap()).await?;
    assert!(!Arc::ptr_eq(&workspace, &second_workspace));
    assert_ne!(workspace.lsp.pid().await, second_workspace.lsp.pid().await);
    // Stopping one workspace leaves other analyzers alive and blocks automatic reattachment.
    let old_pid = workspace.lsp.pid().await;
    let disconnected = cli_call(&root, &base, &["workspace-disconnect"]).await?;
    assert_eq!(disconnected["connected"], false);
    assert_eq!(workspace.lsp.pid().await, None);
    assert!(workspace.open("src/lib.rs").await.is_err());
    assert!(core.workspace(root_str).await.is_err());
    assert!(second_workspace.lsp.pid().await.is_some());
    let status = cli_call(&root, &base, &["workspace-status", "--all"]).await?;
    assert_eq!(status["workspaces"].as_array().unwrap().len(), 1);
    assert!(
        status["disconnectedWorkspaces"]
            .as_array()
            .unwrap()
            .contains(&json!(root_str))
    );
    for _ in 0..3 {
        let reconnect = reqwest::Client::new()
            .post(format!("{base}/api/companion"))
            .json(&event(root_str, "connect", None, None, 0))
            .send()
            .await?;
        assert_eq!(reconnect.status(), 400);
    }
    assert!(core.workspace(root_str).await.is_err());
    let resumed = cli_call(&root, &base, &["workspace-connect"]).await?;
    assert_ne!(resumed["analyzerPid"], json!(old_pid));
    let resumed_workspace = core.workspace(root_str).await?;
    wait_ready(&resumed_workspace).await?;
    assert!(
        cli_call(&root, &base, &["document-symbols", "--path", "src/lib.rs"])
            .await?
            .to_string()
            .contains("agent_wins")
    );
    let reconnected = reqwest::Client::new()
        .post(format!("{base}/api/companion"))
        .json(&event(root_str, "connect", None, None, 0))
        .send()
        .await?;
    assert!(reconnected.status().is_success());
    // A running Cargo check and its build-script child must stop as well.
    std::fs::write(
        second.path().join("build.rs"),
        r#"fn main() {
        std::fs::write("check-started", std::process::id().to_string()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
    }"#,
    )?;
    second_workspace
        .disk_changed(&second.path().join("build.rs"))
        .await?;
    let checking = second_workspace.clone();
    let check = tokio::spawn(async move { checking.check().await });
    tokio::time::timeout(Duration::from_secs(30), async {
        while !second.path().join("check-started").exists() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await?;
    let child_pid: i32 = std::fs::read_to_string(second.path().join("check-started"))?.parse()?;
    tokio::time::timeout(
        Duration::from_secs(5),
        core.disconnect(second.path().to_str().unwrap()),
    )
    .await??;
    tokio::time::timeout(Duration::from_secs(5), check).await???;
    assert_eq!(second_workspace.status().await["check"]["running"], false);
    assert_eq!(second_workspace.lsp.pid().await, None);
    #[cfg(unix)]
    tokio::time::timeout(Duration::from_secs(5), async {
        // SAFETY: signal 0 only checks whether the observed test child exists.
        while unsafe { libc::kill(child_pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    handle.abort();
    core.shutdown().await;
    Ok(())
}

struct TestApplication(Application);
impl TestApplication {
    async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        Ok(self.0.execute(Request::decode(name, args)?).await?)
    }
}

async fn cli_call(cwd: &std::path::Path, endpoint: &str, args: &[&str]) -> Result<Value> {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_bridge"))
        .current_dir(cwd)
        .args(args)
        .args(["--endpoint", endpoint])
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
async fn mcp_call(endpoint: &str, name: &str, arguments: Value) -> Result<Value> {
    let response: Value = reqwest::Client::new().post(endpoint).header("accept", "application/json, text/event-stream").header("mcp-protocol-version", "2025-03-26")
        .json(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":name,"arguments":arguments}})).send().await?.json().await?;
    anyhow::ensure!(
        response.get("error").is_none() && response["result"]["isError"] != true,
        "MCP failed: {response}"
    );
    Ok(serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .context("Missing MCP text")?,
    )?)
}
