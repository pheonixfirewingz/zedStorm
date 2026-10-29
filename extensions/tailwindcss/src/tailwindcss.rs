use std::{env, fs};
use zed::settings::LspSettings;
use zed_extension_api::{
    self as zed, LanguageServerId, Result,
    serde_json::{Value, json},
};

const PACKAGE_NAME: &str = "@tailwindcss/language-server";
const TAILWIND_SERVER_ID: &str = "tailwindcss-language-server";
const TAILWIND_CSS_SERVER_ID: &str = "tailwindcss-intellisense-css";

struct TailwindExtension {
    cached_binary_path: Option<String>,
}

impl TailwindExtension {
    fn server_script_path(
        &mut self,
        language_server_id: &LanguageServerId,
        server_subpath: &str,
    ) -> Result<String> {
        let server_path = format!("node_modules/{PACKAGE_NAME}/{server_subpath}");
        let server_exists = fs::metadata(&server_path).is_ok_and(|stat| stat.is_file());
        if self.cached_binary_path.is_some() && server_exists {
            return Ok(server_path);
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let version = zed::npm_package_latest_version(PACKAGE_NAME)?;

        if !server_exists
            || zed::npm_package_installed_version(PACKAGE_NAME)?.as_ref() != Some(&version)
        {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
            let result = zed::npm_install_package(PACKAGE_NAME, &version);
            match result {
                Ok(()) => {
                    if !fs::metadata(&server_path).is_ok_and(|stat| stat.is_file()) {
                        return Err(format!(
                            "installed package '{PACKAGE_NAME}' did not contain expected path '{server_path}'"
                        ));
                    }
                }
                Err(error) => {
                    if !fs::metadata(&server_path).is_ok_and(|stat| stat.is_file()) {
                        return Err(error);
                    }
                }
            }
        }
        Ok(server_path)
    }
}

impl zed::Extension for TailwindExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let (binary_name, server_subpath) = match language_server_id.as_ref() {
            TAILWIND_SERVER_ID => (
                "tailwindcss-language-server",
                "bin/tailwindcss-language-server",
            ),
            TAILWIND_CSS_SERVER_ID => ("tailwindcss-intellisense-css", "bin/css-language-server"),
            _ => return Err(format!("unknown language server id: {language_server_id}")),
        };

        if let Some(path) = worktree.which(binary_name) {
            return Ok(zed::Command {
                command: path,
                args: vec!["--stdio".to_string()],
                env: Default::default(),
            });
        }

        let server_path = self.server_script_path(language_server_id, server_subpath)?;
        let full_server_path = env::current_dir()
            .map_err(|err| err.to_string())?
            .join(&server_path)
            .to_string_lossy()
            .to_string();

        self.cached_binary_path = Some(full_server_path.clone());

        Ok(zed::Command {
            command: zed::node_binary_path()?,
            args: vec![full_server_path, "--stdio".to_string()],
            env: Default::default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<Value>> {
        let user_options = LspSettings::for_worktree(server_id.as_ref(), worktree)
            .ok()
            .and_then(|lsp_settings| lsp_settings.initialization_options);

        let mut options = json!({
            "provideFormatter": true,
        });

        if let Some(user_options) = user_options {
            merge_json_value(&mut options, &user_options);
        }

        Ok(Some(options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<Value>> {
        match server_id.as_ref() {
            TAILWIND_SERVER_ID => {
                let mut tailwind_user_settings =
                    LspSettings::for_worktree(server_id.as_ref(), worktree)
                        .ok()
                        .and_then(|s| s.settings)
                        .unwrap_or_else(|| json!({}));

                if tailwind_user_settings.get("emmetCompletions").is_none() {
                    tailwind_user_settings["emmetCompletions"] = json!(true);
                }

                if tailwind_user_settings.get("includeLanguages").is_none() {
                    tailwind_user_settings["includeLanguages"] = json!({
                        "html": "html",
                        "css": "css",
                        "javascript": "javascript",
                        "typescript": "typescript",
                        "typescriptreact": "typescriptreact",
                    });
                }

                Ok(Some(json!({
                    "tailwindCSS": tailwind_user_settings
                })))
            }
            TAILWIND_CSS_SERVER_ID => {
                let mut default_config = json!({
                    "css": { "lint": {} },
                    "less": { "lint": {} },
                    "scss": { "lint": {} }
                });

                if let Ok(lsp_settings) = LspSettings::for_worktree(server_id.as_ref(), worktree) {
                    if let Some(user_settings) = lsp_settings.settings {
                        merge_json_value(&mut default_config, &user_settings);
                    }
                }

                Ok(Some(default_config))
            }
            _ => Ok(None),
        }
    }
}

fn merge_json_value(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                merge_json_value(
                    target_map.entry(key.clone()).or_insert(Value::Null),
                    source_value,
                );
            }
        }
        (target, source) => {
            *target = source.clone();
        }
    }
}

zed::register_extension!(TailwindExtension);
