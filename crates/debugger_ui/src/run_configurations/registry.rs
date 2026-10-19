use std::path::Path;
use anyhow::{Result, ensure};
use ui::IconName;

use super::{BeforeLaunchTask, NpmConfigurationFields, RunConfiguration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationTypeId {
    Npm,
    Node,
    JavaScriptDebug,
    AttachNode,
    Compound,
    Cargo,
    Jest,
    Vitest,
    Mocha,
    Playwright,
    NodeTestRunner,
    Grunt,
    Gulp,
    NWJs,
    Protractor,
    ReactNative,
    Nodeunit,
}

pub struct ConfigurationProvider {
    pub id: &'static str,
    pub display_name: &'static str,
    pub icon: IconName,
    pub supported: bool,
    pub unsupported_reason: Option<&'static str>,
}

impl ConfigurationProvider {
    pub fn all() -> &'static [ConfigurationProvider] {
        &[
            ConfigurationProvider {
                id: "npm",
                display_name: "npm",
                icon: IconName::Terminal,
                supported: true,
                unsupported_reason: None,
            },
            ConfigurationProvider {
                id: "node",
                display_name: "Node.js",
                icon: IconName::Code,
                supported: true,
                unsupported_reason: None,
            },
            ConfigurationProvider {
                id: "javascript_debug",
                display_name: "JavaScript Debug",
                icon: IconName::Debug,
                supported: true,
                unsupported_reason: None,
            },
            ConfigurationProvider {
                id: "attach_node",
                display_name: "Attach to Node.js/Chrome",
                icon: IconName::Debug,
                supported: true,
                unsupported_reason: None,
            },
            ConfigurationProvider {
                id: "compound",
                display_name: "Compound",
                icon: IconName::Folder,
                supported: true,
                unsupported_reason: None,
            },
            ConfigurationProvider {
                id: "cargo",
                display_name: "Cargo",
                icon: IconName::Hammer,
                supported: true,
                unsupported_reason: None,
            },
            ConfigurationProvider {
                id: "jest",
                display_name: "Jest",
                icon: IconName::Check,
                supported: false,
                unsupported_reason: Some(
                    "Jest test runner requires jest installed in node_modules or configure an npm test script",
                ),
            },
            ConfigurationProvider {
                id: "vitest",
                display_name: "Vitest",
                icon: IconName::Check,
                supported: false,
                unsupported_reason: Some(
                    "Vitest test runner requires vitest installed in node_modules or configure an npm test script",
                ),
            },
            ConfigurationProvider {
                id: "mocha",
                display_name: "Mocha",
                icon: IconName::Check,
                supported: false,
                unsupported_reason: Some(
                    "Mocha test runner requires mocha installed in node_modules or configure an npm test script",
                ),
            },
            ConfigurationProvider {
                id: "playwright",
                display_name: "Playwright",
                icon: IconName::Check,
                supported: false,
                unsupported_reason: Some(
                    "Playwright requires @playwright/test installed or an npm test script",
                ),
            },
            ConfigurationProvider {
                id: "node_test_runner",
                display_name: "Node.js Test Runner",
                icon: IconName::Check,
                supported: true,
                unsupported_reason: None,
            },
            ConfigurationProvider {
                id: "grunt",
                display_name: "Grunt.js",
                icon: IconName::Hammer,
                supported: false,
                unsupported_reason: Some(
                    "Grunt requires grunt-cli installed in PATH or node_modules",
                ),
            },
            ConfigurationProvider {
                id: "gulp",
                display_name: "Gulp.js",
                icon: IconName::PlayFilled,
                supported: false,
                unsupported_reason: Some(
                    "Gulp requires gulp-cli installed in PATH or node_modules",
                ),
            },
            ConfigurationProvider {
                id: "nwjs",
                display_name: "NW.js",
                icon: IconName::Public,
                supported: false,
                unsupported_reason: Some(
                    "NW.js runtime SDK not detected in environment PATH",
                ),
            },
            ConfigurationProvider {
                id: "protractor",
                display_name: "Protractor",
                icon: IconName::Check,
                supported: false,
                unsupported_reason: Some(
                    "Protractor is deprecated and not installed in this project",
                ),
            },
            ConfigurationProvider {
                id: "react_native",
                display_name: "React Native",
                icon: IconName::PlayFilled,
                supported: false,
                unsupported_reason: Some(
                    "React Native CLI or Expo not detected in this project",
                ),
            },
            ConfigurationProvider {
                id: "nodeunit",
                display_name: "Nodeunit",
                icon: IconName::Check,
                supported: false,
                unsupported_reason: Some(
                    "Nodeunit is deprecated and not installed in this project",
                ),
            },
        ]
    }

    pub fn find_by_id(id: &str) -> Option<&'static ConfigurationProvider> {
        Self::all().iter().find(|p| p.id == id)
    }

    pub fn icon_for_type(type_id: &str) -> IconName {
        Self::find_by_id(type_id)
            .map(|p| p.icon)
            .unwrap_or(IconName::PlayFilled)
    }

    pub fn display_name_for_type(type_id: &str) -> &'static str {
        Self::find_by_id(type_id)
            .map(|p| p.display_name)
            .unwrap_or("Run")
    }

    pub fn create_default(type_id: &str, root: &Path, package_json: Option<&Path>) -> RunConfiguration {
        match type_id {
            "npm" => {
                let pkg_str = package_json
                    .map(|p| {
                        p.strip_prefix(root)
                            .unwrap_or(p)
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_else(|| "package.json".into());
                RunConfiguration {
                    name: "npm".into(),
                    configuration_type: Some("npm".into()),
                    folder: None,
                    allow_multiple_instances: false,
                    before_launch: Vec::new(),
                    npm: Some(NpmConfigurationFields {
                        package_json: pkg_str,
                        command: "run".into(),
                        script: "start".into(),
                        arguments: String::new(),
                        node_runtime: None,
                        node_options: String::new(),
                        package_manager: None,
                        environment: Default::default(),
                        browser_live_edit: None,
                    }),
                    run: None,
                    debug: None,
                }
            }
            "node" => RunConfiguration {
                name: "Node.js".into(),
                configuration_type: Some("node".into()),
                folder: None,
                allow_multiple_instances: false,
                before_launch: Vec::new(),
                npm: None,
                run: Some(task::TaskTemplate {
                    label: "Node.js".into(),
                    command: "node".into(),
                    args: vec!["index.js".into()],
                    cwd: Some(root.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
                debug: Some(task::DebugScenario {
                    adapter: "JavaScript".into(),
                    label: "Node.js".into(),
                    build: None,
                    config: serde_json::json!({
                        "request": "launch",
                        "type": "pwa-node",
                        "program": root.join("index.js").to_string_lossy(),
                        "cwd": root,
                    }),
                    tcp_connection: None,
                }),
            },
            "javascript_debug" => RunConfiguration {
                name: "JavaScript Debug".into(),
                configuration_type: Some("javascript_debug".into()),
                folder: None,
                allow_multiple_instances: false,
                before_launch: Vec::new(),
                npm: None,
                run: None,
                debug: Some(task::DebugScenario {
                    adapter: "JavaScript".into(),
                    label: "JavaScript Debug".into(),
                    build: None,
                    config: serde_json::json!({
                        "request": "launch",
                        "type": "pwa-chrome",
                        "url": "http://localhost:3000",
                        "webRoot": root,
                    }),
                    tcp_connection: None,
                }),
            },
            "attach_node" => RunConfiguration {
                name: "Attach to Node.js/Chrome".into(),
                configuration_type: Some("attach_node".into()),
                folder: None,
                allow_multiple_instances: false,
                before_launch: Vec::new(),
                npm: None,
                run: None,
                debug: Some(task::DebugScenario {
                    adapter: "JavaScript".into(),
                    label: "Attach to Node.js/Chrome".into(),
                    build: None,
                    config: serde_json::json!({
                        "request": "attach",
                        "type": "pwa-node",
                        "port": 9229,
                        "address": "localhost",
                        "restart": true,
                    }),
                    tcp_connection: None,
                }),
            },
            "compound" => RunConfiguration {
                name: "Compound".into(),
                configuration_type: Some("compound".into()),
                folder: None,
                allow_multiple_instances: false,
                before_launch: Vec::new(),
                npm: None,
                run: None,
                debug: None,
            },
            "cargo" => RunConfiguration {
                name: "Cargo".into(),
                configuration_type: Some("cargo".into()),
                folder: None,
                allow_multiple_instances: false,
                before_launch: vec![BeforeLaunchTask::Build {
                    name: "cargo build".into(),
                    target: None,
                }],
                npm: None,
                run: Some(task::TaskTemplate {
                    label: "Cargo".into(),
                    command: "cargo".into(),
                    args: vec!["run".into()],
                    cwd: Some(root.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
                debug: Some(task::DebugScenario {
                    adapter: "CodeLLDB".into(),
                    label: "Cargo".into(),
                    build: Some(task::BuildTaskDefinition::Template {
                        task_template: task::TaskTemplate {
                            label: "cargo build".into(),
                            command: "cargo".into(),
                            args: vec!["build".into()],
                            cwd: Some(root.to_string_lossy().into_owned()),
                            ..Default::default()
                        },
                        locator_name: Some("rust-cargo-locator".into()),
                    }),
                    config: serde_json::json!({
                        "request": "launch",
                        "cwd": root,
                        "sourceLanguages": ["rust"],
                    }),
                    tcp_connection: None,
                }),
            },

            _ => RunConfiguration {
                name: Self::display_name_for_type(type_id).to_string(),
                configuration_type: Some(type_id.into()),
                folder: None,
                allow_multiple_instances: false,
                before_launch: Vec::new(),
                npm: None,
                run: Some(task::TaskTemplate {
                    label: Self::display_name_for_type(type_id).into(),
                    command: "echo".into(),
                    args: vec!["Unsupported configuration".into()],
                    cwd: Some(root.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
                debug: None,
            },
        }
    }

    pub fn validate(config: &RunConfiguration) -> Result<()> {
        ensure!(!config.name.trim().is_empty(), "Enter a configuration name");
        if let Some(npm) = &config.npm {
            ensure!(!npm.package_json.trim().is_empty(), "Choose a package.json path");
            ensure!(!npm.command.trim().is_empty(), "Enter an npm command");
            ensure!(!npm.script.trim().is_empty(), "Choose a script to run from package.json");
            return Ok(());
        }
        if let Some(type_id) = &config.configuration_type {
            if let Some(provider) = Self::find_by_id(type_id) {
                if !provider.supported {
                    if let Some(reason) = provider.unsupported_reason {
                        anyhow::bail!("{}: {reason}", provider.display_name);
                    }
                }
            }
        }
        ensure!(
            config.run.is_some() || config.debug.is_some() || config.configuration_type.as_deref() == Some("compound"),
            "Add a Run or Debug target"
        );
        Ok(())
    }
}
