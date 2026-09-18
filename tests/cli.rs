use anyhow::Result;
use mcp_editor_lsp_bridge::application::{Request, operations};
use serde_json::{Value, json};
use std::{path::Path, process::Output};
use tokio::{io::AsyncWriteExt, process::Command};

async fn cli(cwd: &Path, args: &[&str], stdin: Option<&str>) -> Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bridge"))
        .current_dir(cwd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .await?;
    } else {
        drop(child.stdin.take());
    }
    Ok(child.wait_with_output().await?)
}
fn value(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
#[tokio::test]
async fn discovery_and_validation_work_without_core() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let help = cli(temp.path(), &["--help"], None).await?;
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("code-actions"));
    let tools = value(cli(temp.path(), &["tools"], None).await?);
    assert_eq!(tools.as_array().unwrap().len(), operations().len());
    for operation in operations() {
        let schema = value(cli(temp.path(), &["schema", operation.name], None).await?);
        assert_eq!(schema, operation.input_schema);
    }
    for args in [
        vec!["definition", "--json", "{bad"],
        vec!["definition", "--stdin", "--json", "{}"],
        vec!["hover", "--line", "-1"],
        vec!["rename", "--json", "{}", "--apply"],
        vec!["workspace-status", "--all", "--workspace", "."],
    ] {
        let output = cli(temp.path(), &args, None).await?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr)?["error"]["code"],
            "INVALID_INPUT"
        );
    }
    assert!(
        Request::decode(
            "definition",
            json!({"workspace":"/tmp","path":"a.rs","line":0})
        )
        .is_err()
    );
    assert!(Request::decode("diagnostics", json!({"workspace":"/tmp","check":"true"})).is_err());
    assert!(Request::decode("workspace_status", json!({"unknown":true})).is_err());
    assert!(Request::decode("code_actions", json!({"workspace":"/tmp","path":"a.rs","line":0,"character":0,"end":{"line":0,"character":0,"typo":1}})).is_err());
    Ok(())
}
#[tokio::test]
async fn client_infers_cargo_workspace_and_preserves_json_and_stdin() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    std::fs::create_dir_all(root.join("member/src/deep"))?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=[\"member\"]\nresolver=\"2\"\n",
    )?;
    std::fs::write(
        root.join("member/Cargo.toml"),
        "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )?;
    std::fs::write(root.join("member/src/lib.rs"), "pub fn hi() {}")?;
    // Echo the typed port request; no core/analyzer is started by this fixture.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let app = axum::Router::new().route(
        "/api/execute",
        axum::routing::post(|axum::Json(request): axum::Json<Request>| async move {
            axum::Json(json!({"data":request}))
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cwd = root.join("member/src/deep");
    let request = value(
        cli(
            &cwd,
            &[
                "definition",
                "--path",
                "../lib.rs",
                "--line",
                "0",
                "--character",
                "7",
                "--endpoint",
                &endpoint,
            ],
            None,
        )
        .await?,
    );
    assert_eq!(request["operation"], "definition");
    assert_eq!(request["arguments"]["workspace"], root.to_str().unwrap());
    assert_eq!(
        request["arguments"]["path"],
        root.join("member/src/lib.rs").to_str().unwrap()
    );
    let input = json!({"path":"member/src/lib.rs","line":0,"character":7,"new_name":"literal_$HOME","apply":false}).to_string();
    let inline = value(
        cli(
            &cwd,
            &["rename", "--json", &input, "--endpoint", &endpoint],
            None,
        )
        .await?,
    );
    let piped = value(
        cli(
            &cwd,
            &["rename", "--stdin", "--endpoint", &endpoint],
            Some(&input),
        )
        .await?,
    );
    assert_eq!(inline, piped);
    assert_eq!(piped["arguments"]["new_name"], "literal_$HOME");
    let all = value(
        cli(
            temp.path(),
            &["workspace-status", "--all", "--endpoint", &endpoint],
            None,
        )
        .await?,
    );
    assert_eq!(all["arguments"]["workspace"], Value::Null);
    let explicit = value(
        cli(
            Path::new("/tmp"),
            &[
                "diagnostics",
                "--workspace",
                root.to_str().unwrap(),
                "--check",
                "--endpoint",
                &endpoint,
            ],
            None,
        )
        .await?,
    );
    assert_eq!(
        explicit["arguments"],
        json!({"workspace":root,"check":true,"path":null,"verbose":null})
    );
    server.abort();
    let unavailable = cli(&cwd, &["diagnostics", "--endpoint", &endpoint], None).await?;
    assert_eq!(unavailable.status.code(), Some(1));
    assert!(unavailable.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&unavailable.stderr)?["error"]["code"],
        "CONNECTION_REFUSED"
    );
    Ok(())
}
