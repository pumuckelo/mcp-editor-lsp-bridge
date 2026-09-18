//! Language-specific policy. The core and transports only depend on these adapters.
use crate::core::Config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
}

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TypeScriptBackend {
    #[default]
    Auto,
    Native,
    Vtsls,
    #[serde(rename = "typescript-language-server")]
    TypeScriptLanguageServer,
}

pub struct ServerSpec {
    pub name: &'static str,
    pub command: String,
    pub args: Vec<String>,
    pub settings: Value,
    pub section: &'static str,
    pub server_status: bool,
}
impl Language {
    pub fn for_path(path: &Path) -> Result<Self> {
        match path.extension().and_then(|x| x.to_str()) {
            Some("rs") => Ok(Self::Rust),
            Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs") => {
                Ok(Self::TypeScript)
            }
            _ => bail!("Unsupported source file: {}", path.display()),
        }
    }
    pub fn default_for(root: &Path) -> Result<Self> {
        if root.join("Cargo.toml").is_file() {
            Ok(Self::Rust)
        } else if ["tsconfig.json", "jsconfig.json", "package.json"]
            .iter()
            .any(|p| root.join(p).is_file())
        {
            Ok(Self::TypeScript)
        } else {
            bail!("Workspace must contain Cargo.toml, tsconfig.json, jsconfig.json or package.json")
        }
    }
    pub fn document_id(path: &Path) -> Result<&'static str> {
        Ok(match path.extension().and_then(|x| x.to_str()) {
            Some("rs") => "rust",
            Some("tsx") => "typescriptreact",
            Some("jsx") => "javascriptreact",
            Some("js" | "mjs" | "cjs") => "javascript",
            _ => {
                Self::for_path(path)?;
                "typescript"
            }
        })
    }
    pub async fn spec(self, root: &Path, config: &Config) -> Result<ServerSpec> {
        match self {
            Self::Rust => {
                let mut settings = config.analyzer_settings.clone();
                if !settings.is_object() {
                    bail!("analyzer_settings must be an object");
                }
                settings["checkOnSave"] = json!(false);
                settings["cargo"]["features"] = if config.all_features {
                    json!("all")
                } else {
                    json!(config.cargo_features)
                };
                settings["cargo"]["noDefaultFeatures"] = json!(config.no_default_features);
                if let Some(target) = &config.cargo_target {
                    settings["cargo"]["target"] = json!(target);
                }
                Ok(ServerSpec {
                    name: "rust-analyzer",
                    command: config.analyzer.clone(),
                    args: vec![],
                    settings,
                    section: "rust-analyzer",
                    server_status: true,
                })
            }
            Self::TypeScript => typescript_spec(root, config).await,
        }
    }
}

fn workspace_dependency(root: &Path, relative: &str) -> Option<PathBuf> {
    for dir in root.ancestors() {
        let path = dir.join("node_modules").join(relative);
        if path.is_file() {
            return Some(path);
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    None
}

/// Compiler selection is independent of the LSP wrapper. Prefer the project compiler.
pub fn typescript_binary(root: &Path, config: &Config) -> Result<String> {
    workspace_dependency(root, ".bin/tsc")
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| config.typescript_analyzer.clone())
        .context("TypeScript not found. Install the project's TypeScript dependencies first")
}

async fn typescript_version(binary: &str, root: &Path, config: &Config) -> Result<u32> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(binary)
            .arg("--version")
            .current_dir(root)
            .envs(&config.environment)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("TypeScript version check timed out")??;
    let version = String::from_utf8_lossy(&output.stdout);
    if output.status.success()
        && let Some(major) = version
            .trim()
            .strip_prefix("Version ")
            .and_then(|v| v.split('.').next())
            .and_then(|v| v.parse().ok())
    {
        return Ok(major);
    }
    bail!(
        "Cannot determine TypeScript version at {binary}: {}",
        version.trim()
    )
}

async fn typescript_spec(root: &Path, config: &Config) -> Result<ServerSpec> {
    let compiler = typescript_binary(root, config)?;
    let major = typescript_version(&compiler, root, config).await?;
    let backend = match config.typescript_backend {
        TypeScriptBackend::Auto if major == 7 => TypeScriptBackend::Native,
        TypeScriptBackend::Auto => TypeScriptBackend::Vtsls,
        backend => backend,
    };
    if backend == TypeScriptBackend::Native {
        if major != 7 {
            bail!(
                "Native LSP requires workspace TypeScript 7; select vtsls or typescript-language-server for TypeScript {major}"
            );
        }
        let binary = config.typescript_analyzer.clone().unwrap_or(compiler);
        if typescript_version(&binary, root, config).await? != 7 {
            bail!("Configured typescript_analyzer must be native TypeScript 7");
        }
        return Ok(ServerSpec {
            name: "native-typescript",
            command: binary,
            args: vec!["--lsp".into(), "--stdio".into()],
            settings: config.typescript_settings.clone(),
            section: "typescript",
            server_status: false,
        });
    }
    if !(4..=6).contains(&major) {
        bail!(
            "The selected wrapper requires TypeScript 4–6, got {major}. Use auto or native for TypeScript 7"
        );
    }
    let tsserver = workspace_dependency(root, "typescript/lib/tsserver.js")
        .context("Workspace TypeScript tsserver.js not found. Install the project's dependencies; the wrapper must use the project SDK")?;
    let tsdk = tsserver.parent().unwrap().canonicalize()?;
    if backend == TypeScriptBackend::TypeScriptLanguageServer {
        let mut settings = config.typescript_language_server_settings.clone();
        if !settings.is_object() || settings.get("tsserver").is_some_and(|v| !v.is_object()) {
            bail!("typescript_language_server_settings and its tsserver entry must be objects");
        }
        settings["tsserver"]["path"] = json!(tsdk.join("tsserver.js"));
        return Ok(ServerSpec {
            name: "typescript-language-server",
            command: wrapper_binary(
                root,
                &config.typescript_language_server,
                "typescript-language-server",
            ),
            args: vec!["--stdio".into()],
            settings,
            section: "typescript",
            server_status: false,
        });
    }
    let mut settings = config.vtsls_settings.clone();
    if !settings.is_object() {
        bail!("vtsls_settings must be an object");
    }
    for key in ["typescript", "vtsls"] {
        if settings.get(key).is_some_and(|value| !value.is_object()) {
            bail!("vtsls_settings.{key} must be an object");
        }
    }
    settings["typescript"]["tsdk"] = json!(tsdk);
    settings["vtsls"]["autoUseWorkspaceTsdk"] = json!(true);
    Ok(ServerSpec {
        name: "vtsls",
        command: wrapper_binary(root, &config.vtsls_analyzer, "vtsls"),
        args: vec!["--stdio".into()],
        settings,
        section: "",
        server_status: false,
    })
}

fn wrapper_binary(root: &Path, configured: &Option<String>, name: &str) -> String {
    configured
        .clone()
        .or_else(|| {
            workspace_dependency(root, &format!(".bin/{name}"))
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| name.into())
}

pub fn infer_root(cwd: &Path) -> Result<PathBuf> {
    for dir in cwd.ancestors() {
        if dir.join("Cargo.toml").is_file() {
            let output = std::process::Command::new("cargo")
                .args(["locate-project", "--workspace", "--message-format", "plain"])
                .current_dir(dir)
                .output()?;
            if !output.status.success() {
                bail!(
                    "Cannot infer Cargo workspace: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return Ok(Path::new(String::from_utf8_lossy(&output.stdout).trim())
                .parent()
                .context("Invalid Cargo manifest")?
                .to_owned());
        }
        if ["tsconfig.json", "jsconfig.json", "package.json"]
            .iter()
            .any(|p| dir.join(p).is_file())
        {
            return Ok(dir.to_owned());
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    bail!("Cannot infer workspace. Pass --workspace /path/to/project")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn languages_and_workspace_boundaries() -> Result<()> {
        for (path, id) in [
            ("a.rs", "rust"),
            ("a.ts", "typescript"),
            ("a.mts", "typescript"),
            ("a.cts", "typescript"),
            ("a.tsx", "typescriptreact"),
            ("a.jsx", "javascriptreact"),
            ("a.cjs", "javascript"),
        ] {
            assert_eq!(Language::document_id(Path::new(path))?, id);
        }
        assert!(Language::for_path(Path::new("Cargo.toml")).is_err());
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        std::fs::write(root.join("package.json"), "{}")?;
        std::fs::create_dir_all(root.join("src/deep"))?;
        assert_eq!(infer_root(&root.join("src/deep"))?, root);
        assert_eq!(Language::default_for(root)?, Language::TypeScript);
        std::fs::create_dir_all(root.join("nested/src"))?;
        std::fs::write(root.join("nested/.git"), "gitdir: elsewhere")?;
        assert!(infer_root(&root.join("nested/src")).is_err());
        assert!(typescript_binary(root, &Config::default()).is_err());
        Ok(())
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn backend_selection_pins_workspace_sdk_and_compiler() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        std::fs::create_dir_all(root.join("node_modules/.bin"))?;
        std::fs::create_dir_all(root.join("node_modules/typescript/lib"))?;
        let compiler = root.join("node_modules/.bin/tsc");
        std::fs::write(&compiler, "#!/bin/sh\necho 'Version 5.9.3'\n")?;
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o755))?;
        std::fs::write(root.join("node_modules/typescript/lib/tsserver.js"), "")?;
        let mut config = Config::default();
        let spec = Language::TypeScript.spec(root, &config).await?;
        assert_eq!(spec.name, "vtsls");
        let sdk = root.join("node_modules/typescript/lib").canonicalize()?;
        assert_eq!(spec.settings["typescript"]["tsdk"], json!(sdk));
        assert_eq!(spec.settings["vtsls"]["autoUseWorkspaceTsdk"], true);
        config.typescript_backend = TypeScriptBackend::TypeScriptLanguageServer;
        config.typescript_language_server_settings =
            json!({"tsserver":{"path":"/wrong/sdk", "useSyntaxServer":"never"}});
        let spec = Language::TypeScript.spec(root, &config).await?;
        assert_eq!(spec.name, "typescript-language-server");
        assert_eq!(
            spec.settings["tsserver"]["path"],
            json!(sdk.join("tsserver.js"))
        );
        assert_eq!(spec.settings["tsserver"]["useSyntaxServer"], "never");
        config.typescript_analyzer = Some("/unrelated/native/compiler".into());
        assert_eq!(
            typescript_binary(root, &config)?,
            compiler.to_string_lossy()
        );
        config.typescript_backend = TypeScriptBackend::Native;
        assert!(Language::TypeScript.spec(root, &config).await.is_err());
        config.typescript_analyzer = None;
        config.typescript_backend = TypeScriptBackend::Auto;
        std::fs::write(&compiler, "#!/bin/sh\necho 'Version 7.0.2'\n")?;
        assert_eq!(
            Language::TypeScript.spec(root, &config).await?.name,
            "native-typescript"
        );
        config.typescript_backend = TypeScriptBackend::Vtsls;
        assert!(Language::TypeScript.spec(root, &config).await.is_err());
        std::fs::create_dir_all(root.join("nested/src"))?;
        std::fs::write(root.join("nested/.git"), "gitdir: elsewhere")?;
        assert!(typescript_binary(&root.join("nested/src"), &Config::default()).is_err());
        Ok(())
    }
}
