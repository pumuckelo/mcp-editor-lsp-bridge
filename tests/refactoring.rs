use anyhow::{Context, Result};
use mcp_editor_lsp_bridge::{
    application::{Application, Request},
    core::{Config, Core, SyncEvent},
    server,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
async fn call(app: &Application, name: &str, args: Value) -> Result<Value> {
    Ok(app.execute(Request::decode(name, args)?).await?)
}
async fn cli(root: &Path, endpoint: &str, args: &[&str]) -> Result<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_bridge"))
            .current_dir(root)
            .args(args)
            .args(["--endpoint", endpoint])
            .output(),
    )
    .await??;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
#[tokio::test]
#[ignore = "Requires native TypeScript 7 at BRIDGE_TYPESCRIPT and local sockets"]
async fn guarded_plans_symbol_selection_and_receipts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    std::fs::write(root.join("package.json"), "{}")?;
    std::fs::write(
        root.join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":true,"target":"ESNext","module":"Preserve"}}"#,
    )?;
    std::fs::write(
        root.join("a.ts"),
        "export function greet(name: string) { return name; }\n",
    )?;
    std::fs::write(
        root.join("b.ts"),
        "import { greet } from './a';\nexport const result = greet('hello');\n",
    )?;
    let core = Core::new(Config {
        typescript_analyzer: Some(
            std::env::var("BRIDGE_TYPESCRIPT").context("Set BRIDGE_TYPESCRIPT")?,
        ),
        ..Default::default()
    });
    let workspace = core.workspace(root.to_str().unwrap()).await?;
    let app = Application::new(core.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let router = server::router(core.clone(), port);
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let endpoint = format!("http://127.0.0.1:{port}");
    let before_a = std::fs::read_to_string(root.join("a.ts"))?;
    let before_b = std::fs::read_to_string(root.join("b.ts"))?;
    let preview = cli(
        &root,
        &endpoint,
        &[
            "rename",
            "--path",
            "a.ts",
            "--symbol",
            "greet",
            "--new-name",
            "welcome",
            "--preview",
        ],
    )
    .await?;
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["fileCount"], 2);
    assert_eq!(preview["editCount"], 3);
    assert!(
        preview["files"][0]["path"]
            .as_str()
            .is_some_and(|p| !Path::new(p).is_absolute())
    );
    assert!(preview["files"][0].get("edits").is_none());
    assert_eq!(std::fs::read_to_string(root.join("a.ts"))?, before_a);
    let plan_id = preview["planId"].as_str().unwrap();
    let applied = cli(
        &root,
        &endpoint,
        &["apply-rename", "--plan-id", plan_id, "--verbose"],
    )
    .await?;
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["files"][0]["edits"][0]["oldText"], "greet");
    assert_eq!(
        std::fs::read_to_string(root.join("b.ts"))?,
        before_b.replace("greet", "welcome")
    );
    // Plain rename applies by default. Position and symbol selectors share the same implementation.
    let applied = cli(
        &root,
        &endpoint,
        &[
            "rename",
            "--path",
            "a.ts",
            "--symbol",
            "welcome",
            "--new-name",
            "greet",
        ],
    )
    .await?;
    assert_eq!(applied["applied"], true);
    assert_eq!(std::fs::read_to_string(root.join("b.ts"))?, before_b);
    let references = cli(
        &root,
        &endpoint,
        &["references", "--path", "a.ts", "--symbol", "greet"],
    )
    .await?;
    assert!(references.to_string().contains("b.ts"));
    let compact = cli(&root, &endpoint, &["workspace-status"]).await?;
    assert!(compact.get("documents").is_none());
    assert!(compact["documentCount"].is_number());
    let verbose = cli(&root, &endpoint, &["workspace-status", "--verbose"]).await?;
    assert!(verbose["documents"].is_array());
    // A previously unopened target changes after preview, before its watcher processes it.
    let make_plan = || json!({"workspace":root,"path":"a.ts","symbol":"greet","new_name":"next","preview":true});
    let preview = call(&app, "rename", make_plan()).await?;
    std::fs::write(root.join("b.ts"), format!("{before_b}// concurrent edit\n"))?;
    let error = call(
        &app,
        "apply_rename",
        json!({"workspace":root,"plan_id":preview["planId"]}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("Stale refactor"), "{error}");
    assert_eq!(std::fs::read_to_string(root.join("a.ts"))?, before_a);
    assert!(std::fs::read_to_string(root.join("b.ts"))?.contains("concurrent edit"));
    std::fs::write(root.join("b.ts"), &before_b)?;
    workspace.disk_changed(&root.join("b.ts")).await?;
    // Editor changes invalidate a plan even when disk did not change.
    let uri = workspace.uri(&root.join("a.ts"))?;
    let event = |kind: &str, text: Option<String>| SyncEvent {
        workspace: root.to_string_lossy().into(),
        companion: "test".into(),
        kind: kind.into(),
        uri: if kind == "connect" || kind == "disconnect" {
            None
        } else {
            Some(uri.clone())
        },
        text,
        epoch: 0,
    };
    workspace.sync(event("connect", None)).await?;
    let preview = call(&app, "rename", make_plan()).await?;
    // Initial edit epoch may have advanced due to earlier renames; use the returned current epoch.
    let status = workspace.status().await;
    let epoch = status["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["uri"] == uri)
        .unwrap()["epoch"]
        .as_u64()
        .unwrap();
    let mut change = event("change", Some(format!("{before_a}// unsaved\n")));
    change.epoch = epoch;
    assert_eq!(workspace.sync(change).await?["accepted"], true);
    assert!(
        call(
            &app,
            "apply_rename",
            json!({"workspace":root,"plan_id":preview["planId"]})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("Stale refactor")
    );
    assert_eq!(std::fs::read_to_string(root.join("a.ts"))?, before_a);
    workspace.sync(event("disconnect", None)).await?;
    // Versioned edits are rejected before touching disk, even with a current snapshot.
    {
        let _guard = workspace.edit_lock.lock().await;
        let snapshot = workspace.snapshot().await?;
        let edit = json!({"documentChanges":[{"textDocument":{"uri":uri,"version":-1},"edits":[{"range":{"start":{"line":0,"character":16},"end":{"line":0,"character":21}},"newText":"wrong"}]}]});
        assert!(
            workspace
                .prepare_edit(&edit, &snapshot)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("Stale LSP document version")
        );
    }
    // Two declarations with the same name return candidates and never mutate either one.
    std::fs::write(
        root.join("ambiguous.ts"),
        "export class A { run() {} }\nexport class B { run() {} }\n",
    )?;
    let result = cli(
        &root,
        &endpoint,
        &[
            "rename",
            "--path",
            "ambiguous.ts",
            "--symbol",
            "run",
            "--new-name",
            "renamed",
        ],
    )
    .await?;
    assert_eq!(result["applied"], false);
    assert_eq!(result["reason"], "ambiguous_symbol");
    assert_eq!(result["candidates"].as_array().unwrap().len(), 2);
    assert!(!std::fs::read_to_string(root.join("ambiguous.ts"))?.contains("renamed"));
    // Created files also invalidate an otherwise unchanged preview.
    let preview = call(&app, "rename", make_plan()).await?;
    std::fs::write(root.join("new.ts"), "export const fresh = 1;\n")?;
    assert!(
        call(
            &app,
            "apply_rename",
            json!({"workspace":root,"plan_id":preview["planId"]})
        )
        .await
        .is_err()
    );
    // Single-use preview: concurrent apply callers cannot both succeed.
    let preview = call(&app, "rename", make_plan()).await?;
    let args = json!({"workspace":root,"plan_id":preview["planId"]});
    let (first, second) = tokio::join!(
        call(&app, "apply_rename", args.clone()),
        call(&app, "apply_rename", args)
    );
    assert_ne!(first.is_ok(), second.is_ok());
    assert!(std::fs::read_to_string(root.join("a.ts"))?.contains("function next"));
    core.shutdown().await;
    handle.abort();
    Ok(())
}
