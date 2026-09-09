//! CLI adapter: discovery and input handling locally; semantic work in the core.
use crate::{
    application::{self, ApiError, ApiResponse, ErrorCode, Request},
    companion,
    core::{Config, Core},
    server,
};
use clap::{Arg, ArgAction, Command};
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Read},
    path::Path,
    process::ExitCode,
    time::Duration,
};

fn command() -> Command {
    let mut root = Command::new("bridge")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Shared Rust and TypeScript semantic tools. JSON results on stdout, errors on stderr. Positions are zero-based UTF-16.")
        .subcommand_required(true).arg_required_else_help(true)
        .arg(Arg::new("endpoint").long("endpoint").global(true).default_value("http://127.0.0.1:47831").help("Running core URL; client calls never launch an analyzer"))
        .subcommand(Command::new("serve").about("Run the shared core, UI and MCP server")
            .arg(Arg::new("port").long("port").default_value("47831").value_parser(clap::value_parser!(u16)))
            .arg(Arg::new("config").long("config"))
            .arg(Arg::new("workspace").long("workspace").action(ArgAction::Append)))
        .subcommand(Command::new("companion").about("Run the editor companion LSP transport"))
        .subcommand(Command::new("tools").about("List operation names and descriptions"))
        .subcommand(Command::new("schema").about("Print one operation's input JSON schema")
            .arg(Arg::new("operation").required(true)));
    for operation in application::operations() {
        let mut cmd = Command::new(operation.name.replace('_', "-"))
            .about(operation.description)
            .arg(
                Arg::new("json")
                    .long("json")
                    .help("Operation arguments as JSON; workspace may be omitted")
                    .conflicts_with("stdin"),
            )
            .arg(
                Arg::new("stdin")
                    .long("stdin")
                    .action(ArgAction::SetTrue)
                    .help("Read JSON arguments from piped stdin"),
            );
        if operation.name.contains('_') {
            cmd = cmd.visible_alias(operation.name);
        }
        for (name, _) in operation.input_schema["properties"].as_object().unwrap() {
            let mut arg = Arg::new(name.clone()).long(name.replace('_', "-"));
            arg = match name.as_str() {
                "line" | "character" => arg.value_parser(clap::value_parser!(u32)),
                "apply" | "check" | "preview" | "verbose" => arg.action(ArgAction::SetTrue),
                "workspace" => arg.help("Workspace directory; inferred from cwd if omitted"),
                "path" => arg.help("File path relative to cwd (JSON paths are workspace-relative)"),
                "end" => arg.help("Range end JSON: {\"line\":0,\"character\":1}"),
                _ => arg,
            };
            if name != "workspace" && name != "verbose" {
                arg = arg.conflicts_with_all(["json", "stdin"]);
            }
            if name == "symbol" {
                arg = arg.conflicts_with_all(["line", "character"]);
            }
            if name == "preview" {
                arg = arg.conflicts_with("apply");
            }
            cmd = cmd.arg(arg);
        }
        if operation.name == "workspace_status" {
            cmd = cmd.arg(
                Arg::new("all")
                    .long("all")
                    .action(ArgAction::SetTrue)
                    .conflicts_with_all(["workspace", "json", "stdin"])
                    .help("List all core sessions without workspace inference"),
            );
        }
        root = root.subcommand(cmd);
    }
    root
}

fn infer_workspace(cwd: &Path) -> Result<String, ApiError> {
    let root = crate::language::infer_root(cwd).map_err(|e| ApiError::invalid(e.to_string()))?;
    canonical_workspace(&root)
}

fn canonical_workspace(path: &Path) -> Result<String, ApiError> {
    let path = path.canonicalize().map_err(|e| {
        ApiError::invalid(format!("Cannot resolve workspace {}: {e}", path.display()))
    })?;
    crate::language::Language::default_for(&path).map_err(|e| ApiError::invalid(e.to_string()))?;
    Ok(path.to_string_lossy().into_owned())
}
fn arguments(name: &str, matches: &clap::ArgMatches) -> Result<Request, ApiError> {
    let mut args = if let Some(text) = matches.get_one::<String>("json") {
        serde_json::from_str::<Value>(text)
            .map_err(|e| ApiError::invalid(format!("Invalid JSON: {e}")))?
    } else if matches.get_flag("stdin") {
        if std::io::stdin().is_terminal() {
            return Err(ApiError::invalid("Pipe JSON into stdin when using --stdin"));
        }
        let mut text = String::new();
        std::io::stdin()
            .take(2 * 1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|e| ApiError::invalid(e.to_string()))?;
        if text.len() > 2 * 1024 * 1024 {
            return Err(ApiError::invalid("JSON input exceeds 2 MB"));
        }
        serde_json::from_str(&text).map_err(|e| ApiError::invalid(format!("Invalid JSON: {e}")))?
    } else {
        let operation = application::operations()
            .into_iter()
            .find(|op| op.name == name)
            .unwrap();
        let mut values = serde_json::Map::new();
        for (field, _) in operation.input_schema["properties"].as_object().unwrap() {
            match field.as_str() {
                "workspace" => {}
                "apply" | "check" | "preview" | "verbose" => {
                    if matches.get_flag(field) {
                        values.insert(field.clone(), json!(true));
                    }
                }
                "line" | "character" => {
                    if let Some(n) = matches.get_one::<u32>(field) {
                        values.insert(field.clone(), json!(n));
                    }
                }
                _ => {
                    if let Some(text) = matches.get_one::<String>(field) {
                        let value = if field == "end" {
                            serde_json::from_str(text).map_err(|e| {
                                ApiError::invalid(format!("Invalid --end JSON: {e}"))
                            })?
                        } else if field == "path" {
                            let path = Path::new(text).canonicalize().map_err(|e| {
                                ApiError::invalid(format!("Cannot resolve file {text}: {e}"))
                            })?;
                            json!(path)
                        } else {
                            json!(text)
                        };
                        values.insert(field.clone(), value);
                    }
                }
            }
        }
        Value::Object(values)
    };
    let args = args
        .as_object_mut()
        .ok_or_else(|| ApiError::invalid("Arguments must be a JSON object"))?;
    if let Some(root) = matches.get_one::<String>("workspace") {
        let workspace = canonical_workspace(Path::new(root))?;
        if let Some(existing) = args.get("workspace") {
            let existing = existing
                .as_str()
                .ok_or_else(|| ApiError::invalid("workspace must be a string"))?;
            if canonical_workspace(Path::new(existing))? != workspace {
                return Err(ApiError::invalid(
                    "--workspace disagrees with JSON workspace",
                ));
            }
        }
        args.insert("workspace".into(), json!(workspace));
    } else if let Some(root) = args.get("workspace").and_then(Value::as_str) {
        args.insert(
            "workspace".into(),
            json!(canonical_workspace(Path::new(root))?),
        );
    } else if !(args.contains_key("workspace")
        || name == "workspace_status" && matches.get_flag("all"))
    {
        let cwd = std::env::current_dir().map_err(|e| ApiError::invalid(e.to_string()))?;
        args.insert("workspace".into(), json!(infer_workspace(&cwd)?));
    }
    if matches.try_get_one::<bool>("verbose").ok().flatten() == Some(&true) {
        if args.get("verbose") == Some(&json!(false)) {
            return Err(ApiError::invalid(
                "--verbose disagrees with JSON verbose=false",
            ));
        }
        args.insert("verbose".into(), json!(true));
    }
    Request::decode(name, json!(args))
}

pub async fn call(endpoint: &str, request: &Request) -> Result<Value, ApiError> {
    let mut url = url::Url::parse(endpoint)
        .map_err(|e| ApiError::invalid(format!("Invalid endpoint: {e}")))?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ApiError::invalid(
            "Endpoint must be a local http://127.0.0.1, localhost, or [::1] URL",
        ));
    }
    url.set_path("/api/execute");
    url.set_query(None);
    url.set_fragment(None);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| ApiError::unavailable(e.to_string()))?;
    let response = client
        .post(url)
        .json(request)
        .send()
        .await
        .map_err(|e| connection_error(endpoint, &e, e.is_timeout()))?;
    let status = response.status();
    match response.json::<ApiResponse>().await.map_err(|e| if e.is_timeout() || e.is_body() { connection_error(endpoint, &e, e.is_timeout()) } else { ApiError::unavailable(format!("Core returned HTTP {status} with an invalid response: {e}. Ensure the running core supports the CLI API.")) })? {
        ApiResponse::Success { data } if status.is_success() => Ok(data),
        ApiResponse::Success { .. } => Err(ApiError::unavailable(format!("Core returned HTTP {status}"))),
        ApiResponse::Failure { error } => Err(error),
    }
}

pub async fn run() -> ExitCode {
    let matches = match command().try_get_matches() {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("{}", json!({"error":ApiError::invalid(error.to_string())}));
            return ExitCode::from(2);
        }
    };
    let (name, options) = matches.subcommand().unwrap();
    let endpoint = matches.get_one::<String>("endpoint").unwrap();
    if name == "companion" {
        // Editor stdin can remain open after exit; terminate Tokio's blocking reader.
        match companion::run(endpoint.clone()).await {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
    }
    let result = if name == "serve" {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .init();
        serve(options)
            .await
            .map(|()| None)
            .map_err(|e| ApiError::unavailable(format!("{e:#}")))
    } else if name == "tools" {
        Ok(Some(json!(
            application::operations()
                .iter()
                .map(|op| json!({"name":op.name,"description":op.description}))
                .collect::<Vec<_>>()
        )))
    } else if name == "schema" {
        let operation = options
            .get_one::<String>("operation")
            .unwrap()
            .replace('-', "_");
        application::operations()
            .into_iter()
            .find(|op| op.name == operation)
            .map(|op| Some(op.input_schema))
            .ok_or_else(|| {
                ApiError::invalid(format!("Unknown operation: {operation}. Run bridge tools."))
            })
    } else {
        match arguments(&name.replace('-', "_"), options) {
            Ok(request) => call(endpoint, &request).await.map(Some),
            Err(error) => Err(error),
        }
    };
    match result {
        Ok(value) => {
            if let Some(value) = value {
                println!("{value}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let code = if matches!(error.code, ErrorCode::InvalidInput) {
                2
            } else {
                1
            };
            eprintln!("{}", json!({"error":error}));
            ExitCode::from(code)
        }
    }
}
async fn serve(options: &clap::ArgMatches) -> anyhow::Result<()> {
    use anyhow::Context;
    let port = *options.get_one::<u16>("port").unwrap();
    let config: Config = match options.get_one::<String>("config") {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
        None => Config::default(),
    };
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .context("Bind local core; another bridge may already be running")?;
    let core = Core::new(config);
    if let Some(workspaces) = options.get_many::<String>("workspace") {
        for root in workspaces {
            core.workspace(root).await?;
        }
    }
    tracing::info!("UI: http://127.0.0.1:{port}  MCP: http://127.0.0.1:{port}/mcp");
    let result = axum::serve(listener, server::router(core.clone(), port))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    core.shutdown().await;
    Ok(result?)
}

fn connection_error(
    endpoint: &str,
    error: &(dyn std::error::Error + 'static),
    timeout: bool,
) -> ApiError {
    let mut current = Some(error);
    let mut causes = vec![];
    let mut kind = None;
    while let Some(error) = current {
        let text = error.to_string();
        if causes.last() != Some(&text) {
            causes.push(text);
        }
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            kind = Some(io.kind());
        }
        current = error.source();
    }
    let (code, guidance) = match kind {
        Some(std::io::ErrorKind::PermissionDenied) => (
            ErrorCode::PermissionDenied,
            "Localhost access was denied by the sandbox or OS. Retry through the harness permission flow; do not start another core.",
        ),
        Some(std::io::ErrorKind::ConnectionRefused) => (
            ErrorCode::ConnectionRefused,
            "No core accepted the connection. Check its endpoint and running state; start bridge serve if stopped.",
        ),
        Some(std::io::ErrorKind::TimedOut) => (
            ErrorCode::TimedOut,
            "The core did not respond in time. A mutation may already have applied; inspect disk before retrying.",
        ),
        _ if timeout => (
            ErrorCode::TimedOut,
            "The core did not respond in time. A mutation may already have applied; inspect disk before retrying.",
        ),
        _ => (
            ErrorCode::Unavailable,
            "Check core availability and sandbox localhost permissions. For a mutation, inspect disk before retrying.",
        ),
    };
    ApiError {
        code,
        message: format!(
            "Cannot reach bridge core at {endpoint}: {}. {guidance}",
            causes.join(": ")
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_connection_failures() {
        for (kind, expected) in [
            (std::io::ErrorKind::PermissionDenied, "PERMISSION_DENIED"),
            (std::io::ErrorKind::ConnectionRefused, "CONNECTION_REFUSED"),
            (std::io::ErrorKind::TimedOut, "TIMED_OUT"),
        ] {
            let error = connection_error("http://127.0.0.1:1", &std::io::Error::from(kind), false);
            assert_eq!(serde_json::to_value(error.code).unwrap(), expected);
            if kind == std::io::ErrorKind::PermissionDenied {
                assert!(!error.message.contains("start bridge serve"));
            }
        }
    }
}
