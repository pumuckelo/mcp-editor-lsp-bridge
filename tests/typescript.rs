use anyhow::{Context, Result};
use mcp_editor_lsp_bridge::{
    core::{Config, Core, SyncEvent},
    server,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

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
#[ignore = "Requires BRIDGE_TYPESCRIPT pointing to native TypeScript 7 tsc, rust-analyzer and local sockets"]
async fn native_typescript_cli_and_companion() -> Result<()> {
    let binary =
        std::env::var("BRIDGE_TYPESCRIPT").context("Set BRIDGE_TYPESCRIPT to TypeScript 7 tsc")?;
    exercise_typescript(binary, Config::default(), None).await
}

#[tokio::test]
#[ignore = "Requires BRIDGE_TYPESCRIPT_PACKAGE, BRIDGE_VTSLS, BRIDGE_TSLS, rust-analyzer and local sockets"]
async fn legacy_typescript_backends_and_switching() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let package = std::env::var("BRIDGE_TYPESCRIPT_PACKAGE")?;
    let config = Config {
        vtsls_analyzer: Some(std::env::var("BRIDGE_VTSLS")?),
        typescript_language_server: Some(std::env::var("BRIDGE_TSLS")?),
        ..Config::default()
    };
    exercise_typescript(format!("{package}/bin/tsc"), config, Some(package)).await
}

async fn exercise_typescript(
    binary: String,
    config: Config,
    package: Option<String>,
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::create_dir_all(root.join("node_modules/.bin"))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&binary, root.join("node_modules/.bin/tsc"))?;
    std::fs::write(root.join("package.json"), "{\"type\":\"module\"}")?;
    std::fs::write(
        root.join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":true,"target":"ESNext","module":"Preserve","allowJs":true,"checkJs":true,"jsx":"preserve"},"include":["src"]}"#,
    )?;
    std::fs::write(
        root.join("src/helper.ts"),
        "export function greet(name: string): string { return name; }\n",
    )?;
    std::fs::write(
        root.join("src/main.ts"),
        "import { greet } from './helper';\nexport const message = greet('world');\n",
    )?;
    std::fs::write(root.join("src/view.tsx"), "export const view = 42;\n")?;
    std::fs::write(root.join("src/plain.jsx"), "export const plain = 42;\n")?;
    if let Some(package) = &package {
        #[cfg(unix)]
        std::os::unix::fs::symlink(package, root.join("node_modules/typescript"))?;
    }
    let core = Core::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let app = server::router(core.clone(), port);
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoint = format!("http://127.0.0.1:{port}");
    let status = cli(&root.join("src"), &endpoint, &["workspace-status"]).await?;
    assert_eq!(status["language"], "typescript");
    assert_eq!(status["analyzers"].as_array().unwrap().len(), 1);
    assert_eq!(status["ready"], true);
    let symbols = cli(
        &root,
        &endpoint,
        &["document-symbols", "--path", "src/helper.ts"],
    )
    .await?;
    assert!(symbols.to_string().contains("greet"), "{symbols}");
    let hover = cli(
        &root,
        &endpoint,
        &[
            "hover",
            "--path",
            "src/helper.ts",
            "--line",
            "0",
            "--character",
            "17",
        ],
    )
    .await?;
    assert!(hover.to_string().contains("string"), "{hover}");
    let definition = cli(
        &root,
        &endpoint,
        &[
            "definition",
            "--path",
            "src/main.ts",
            "--line",
            "1",
            "--character",
            "24",
        ],
    )
    .await?;
    assert!(definition.to_string().contains("helper.ts"), "{definition}");
    let references = cli(
        &root,
        &endpoint,
        &[
            "references",
            "--path",
            "src/helper.ts",
            "--line",
            "0",
            "--character",
            "17",
        ],
    )
    .await?;
    assert!(references.to_string().contains("main.ts"), "{references}");
    let preview = cli(
        &root,
        &endpoint,
        &[
            "rename",
            "--json",
            r#"{"path":"src/helper.ts","line":0,"character":17,"new_name":"welcome","preview":true}"#,
        ],
    )
    .await?;
    assert!(preview.to_string().contains("welcome"));
    assert!(std::fs::read_to_string(root.join("src/helper.ts"))?.contains("greet"));
    let applied = cli(
        &root,
        &endpoint,
        &[
            "rename",
            "--json",
            r#"{"path":"src/helper.ts","line":0,"character":17,"new_name":"welcome","apply":true}"#,
        ],
    )
    .await?;
    assert_eq!(applied["applied"], true);
    assert!(std::fs::read_to_string(root.join("src/helper.ts"))?.contains("function welcome"));
    assert!(std::fs::read_to_string(root.join("src/main.ts"))?.contains("welcome('world')"));
    for file in ["src/view.tsx", "src/plain.jsx"] {
        assert!(
            !cli(&root, &endpoint, &["document-symbols", "--path", file])
                .await?
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    let actions = cli(
        &root,
        &endpoint,
        &[
            "code-actions",
            "--path",
            "src/helper.ts",
            "--line",
            "0",
            "--character",
            "17",
        ],
    )
    .await?;
    assert!(actions.is_array(), "{actions}");
    let symbols = cli(
        &root,
        &endpoint,
        &["workspace-symbols", "--query", "welcome"],
    )
    .await?;
    assert!(symbols.to_string().contains("welcome"), "{symbols}");
    let checked = cli(&root, &endpoint, &["diagnostics", "--check"]).await?;
    assert_eq!(checked["savedFileCheck"]["success"], true, "{checked}");
    let mut workspace = core.workspace(root.to_str().unwrap()).await?;
    // Real companion executable forwards TS buffers through exactly the editor transport.
    use mcp_editor_lsp_bridge::protocol::{read_message, write_message};
    let mut companion = tokio::process::Command::new(env!("CARGO_BIN_EXE_bridge"))
        .args(["companion", "--endpoint", &endpoint])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut writer = companion.stdin.take().unwrap();
    let mut reader = tokio::io::BufReader::new(companion.stdout.take().unwrap());
    write_message(&mut writer, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":url::Url::from_directory_path(&root).unwrap().as_str()}})).await?;
    assert!(read_message(&mut reader).await?.unwrap()["result"].is_object());
    let uri = workspace.uri(&root.join("src/view.tsx"))?;
    write_message(&mut writer, &json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"typescriptreact","version":1,"text":"export const overlay: string = 42;\n"}}})).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let symbols = cli(
                &root,
                &endpoint,
                &["document-symbols", "--path", "src/view.tsx"],
            )
            .await?;
            if symbols.to_string().contains("overlay") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    async fn wait_for_error(root: &Path, endpoint: &str, uri: &str) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let diagnostics = cli(root, endpoint, &["diagnostics"]).await?;
                if diagnostics["liveDiagnostics"][uri]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await??;
        Ok(())
    }
    wait_for_error(&root, &endpoint, &uri).await?;
    if package.is_some() {
        // Failed selection leaves the current analyzer and unsaved buffer intact.
        assert!(
            core.set_typescript_backend(
                root.to_str().unwrap(),
                mcp_editor_lsp_bridge::language::TypeScriptBackend::Native
            )
            .await
            .is_err()
        );
        assert!(workspace.lsp.pid().await.is_some());
        cli(
            &root,
            &endpoint,
            &[
                "workspace-backend",
                "--backend",
                "typescript-language-server",
            ],
        )
        .await?;
        workspace = core.workspace(root.to_str().unwrap()).await?;
        assert!(workspace.status().await["companion"].is_string());
        wait_for_error(&root, &endpoint, &uri).await?;
        let renamed = cli(
            &root,
            &endpoint,
            &[
                "rename",
                "--path",
                "src/helper.ts",
                "--symbol",
                "welcome",
                "--new-name",
                "hello",
            ],
        )
        .await?;
        assert_eq!(renamed["applied"], true);
        assert!(std::fs::read_to_string(root.join("src/main.ts"))?.contains("hello('world')"));
        cli(
            &root,
            &endpoint,
            &["workspace-backend", "--backend", "vtsls"],
        )
        .await?;
        workspace = core.workspace(root.to_str().unwrap()).await?;
        wait_for_error(&root, &endpoint, &uri).await?;
    }
    assert!(std::fs::read_to_string(root.join("src/view.tsx"))?.contains("view = 42"));
    // Disk wins over the unsaved overlay; watcher refreshes the running TS server.
    std::fs::write(
        root.join("src/view.tsx"),
        "export const diskWins: string = 'saved';\n",
    )?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let symbols = workspace
                .lsp
                .request(
                    "textDocument/documentSymbol",
                    json!({"textDocument":{"uri":uri}}),
                )
                .await?;
            if symbols.to_string().contains("diskWins") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    let diagnostics = cli(&root, &endpoint, &["diagnostics", "--check", "--verbose"]).await?;
    assert_eq!(
        diagnostics["savedFileCheck"]["success"], true,
        "{diagnostics}"
    );
    assert!(
        diagnostics["liveDiagnostics"][&uri]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    std::fs::write(
        root.join("src/view.tsx"),
        "export const wrong: string = 42;\n",
    )?;
    workspace.disk_changed(&root.join("src/view.tsx")).await?;
    let diagnostics = cli(&root, &endpoint, &["diagnostics", "--check", "--verbose"]).await?;
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
    write_message(&mut writer, &json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri}}})).await?;
    write_message(&mut writer, &json!({"jsonrpc":"2.0","method":"exit"})).await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), companion.wait())
            .await??
            .success()
    );
    assert!(workspace.status().await["companion"].is_null());
    // Mixed workspace lazily adds Rust without replacing the existing TS server.
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"mixed_fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )?;
    std::fs::write(root.join("src/lib.rs"), "pub fn rust_symbol() {}\n")?;
    let rust = cli(
        &root,
        &endpoint,
        &["document-symbols", "--path", "src/lib.rs"],
    )
    .await?;
    assert!(rust.to_string().contains("rust_symbol"), "{rust}");
    let status = workspace.status().await;
    assert_eq!(status["analyzers"].as_array().unwrap().len(), 2);
    assert_eq!(status["analyzerPid"], workspace.lsp.pid().await.unwrap());
    let rust_server = workspace.language_server("src/lib.rs").await?;
    cli(&root, &endpoint, &["workspace-disconnect"]).await?;
    assert_eq!(workspace.lsp.pid().await, None);
    assert_eq!(rust_server.pid().await, None);
    assert!(core.workspace(root.to_str().unwrap()).await.is_err());
    assert!(
        workspace
            .sync(SyncEvent {
                workspace: root.to_string_lossy().into(),
                companion: "test".into(),
                kind: "connect".into(),
                uri: None,
                text: None,
                epoch: 0
            })
            .await
            .is_err()
    );
    // Reconnecting now starts Rust first; opening TS adds only one TS server.
    let resumed = core.connect(root.to_str().unwrap()).await?;
    assert_eq!(resumed.status().await["language"], "rust");
    if package.is_some() {
        assert_eq!(
            resumed.status().await["config"]["typescript_backend"],
            "vtsls"
        );
    }
    resumed.open("src/helper.ts").await?;
    resumed.open("src/main.ts").await?;
    assert_eq!(
        resumed.status().await["analyzers"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let checks = resumed.diagnostics(true).await?;
    assert_eq!(checks["savedFileCheck"]["success"], false, "{checks}");
    assert!(
        checks["savedFileCheck"]["diagnostics"]
            .to_string()
            .contains("typescript")
    );
    core.shutdown().await;
    handle.abort();
    Ok(())
}
