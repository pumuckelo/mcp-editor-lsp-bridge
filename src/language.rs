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

pub struct ServerSpec {
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
    pub fn spec(self, root: &Path, config: &Config) -> Result<ServerSpec> {
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
                    command: config.analyzer.clone(),
                    args: vec![],
                    settings,
                    section: "rust-analyzer",
                    server_status: true,
                })
            }
            Self::TypeScript => Ok(ServerSpec {
                command: typescript_binary(root, config)?,
                args: vec!["--lsp".into(), "--stdio".into()],
                settings: config.typescript_settings.clone(),
                section: "typescript",
                server_status: false,
            }),
        }
    }
}
pub fn typescript_binary(root: &Path, config: &Config) -> Result<String> {
    if let Some(binary) = &config.typescript_analyzer {
        return Ok(binary.clone());
    }
    // Search hoisted dependencies, but never borrow an installation across a Git boundary.
    for dir in root.ancestors() {
        let path = dir.join("node_modules/.bin/tsc");
        if path.is_file() {
            return Ok(path.to_string_lossy().into());
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    bail!(
        "Native TypeScript 7 not found. Install typescript@^7 in this workspace or set typescript_analyzer to its tsc executable"
    )
}
pub async fn verify_typescript(binary: &str, root: &Path, config: &Config) -> Result<()> {
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
    if !output.status.success() || !version.trim().starts_with("Version 7.") {
        bail!(
            "Expected native TypeScript 7 at {binary}; got {}",
            version.trim()
        );
    }
    Ok(())
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
    async fn reject_legacy_typescript() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let binary = temp.path().join("tsc");
        std::fs::write(&binary, "#!/bin/sh\necho 'Version 5.9.3'\n")?;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))?;
        let error = verify_typescript(binary.to_str().unwrap(), temp.path(), &Config::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Expected native TypeScript 7"));
        Ok(())
    }
}
