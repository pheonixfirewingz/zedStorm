mod configuration_editor;
pub mod registry;

use anyhow::{Context as _, Result, ensure};
use collections::{BTreeMap, HashMap};
use dap::DapRegistry;
use db::kvp::KeyValueStore;
use fs::Fs;
use futures::{FutureExt as _, future::Shared};
use gpui::{
    Action as _, App, AppContext, Context, Entity, Subscription, Task, WeakEntity, Window, actions,
};
use project::{Project, TaskContexts, TaskSourceKind, WorktreeId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use task::{DebugScenario, ResolvedTask, TaskId, TaskTemplate};
use ui::IconName;
use util::ResultExt;
use workspace::{Workspace, notifications::DetachAndPromptErr};

use crate::debugger_panel::DebugPanel;
pub use configuration_editor::ConfigurationEditor;

actions!(
    run,
    [
        /// Edits the project's run and debug configurations.
        EditConfigurations,
        /// Runs the selected configuration.
        RunSelected,
        /// Builds the selected configuration without running or debugging it.
        BuildSelected,
        /// Debugs the selected configuration.
        DebugSelected,
        /// Stops runs and builds for the selected configuration.
        StopSelectedRun,
        /// Stops the selected debug context.
        StopSelectedDebug,
        /// Prompts to choose a configuration or script to run.
        RunPrompt,
        /// Prompts to choose a configuration or script to debug.
        DebugPrompt,
        /// Stops all background processes.
        StopAllProcesses,
        /// Shows the list of running configurations and processes.
        ShowRunningList,
        /// Manage execution targets.
        ManageTargets,
        /// Runs an npm/gulp/grunt task.
        RunNpmTask,
        /// Runs the selected configuration with coverage.
        RunWithCoverage,
        /// Profiles the selected configuration.
        Profile,
    ]
);

const CONFIGURATION_PATH: &str = ".zed/run.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunConfiguration {
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub debug_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_multiple_instances: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_launch: Vec<BeforeLaunchTask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm: Option<NpmConfigurationFields>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<TaskTemplate>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_debug_target"
    )]
    pub debug: Option<DebugScenario>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BeforeLaunchTask {
    #[serde(rename = "build")]
    Build {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    #[serde(rename = "npm")]
    NpmScript {
        script: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package_json: Option<String>,
    },
    #[serde(rename = "run_configuration")]
    RunConfiguration {
        configuration_id: String,
    },
}

impl BeforeLaunchTask {
    pub fn label(&self) -> String {
        match self {
            Self::Build { name, target } => {
                if let Some(target) = target {
                    format!("Build {name} ({target})")
                } else {
                    format!("Build {name}")
                }
            }
            Self::NpmScript { script, package_json } => {
                if let Some(pkg) = package_json {
                    format!("Run npm script '{script}' in {pkg}")
                } else {
                    format!("Run npm script '{script}'")
                }
            }
            Self::RunConfiguration { configuration_id } => {
                format!("Run configuration '{configuration_id}'")
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NpmConfigurationFields {
    pub package_json: String,
    #[serde(default = "default_npm_command")]
    pub command: String,
    pub script: String,
    #[serde(default)]
    pub arguments: String,
    #[serde(default)]
    pub node_runtime: Option<String>,
    #[serde(default)]
    pub node_options: String,
    #[serde(default)]
    pub package_manager: Option<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_live_edit: Option<BrowserLiveEdit>,
}

fn default_npm_command() -> String {
    "run".into()
}

impl NpmConfigurationFields {
    fn launch_command(&self, root: &Path, build: bool) -> Result<NpmLaunchCommand> {
        let package_path = root.join(&self.package_json);
        let directory = package_path
            .parent()
            .context("The package.json path has no parent directory")?
            .to_path_buf();
        let manager = self
            .package_manager
            .as_deref()
            .filter(|manager| !manager.trim().is_empty() && !manager.starts_with("Project"))
            .unwrap_or("npm");
        let manager = if cfg!(windows) && matches!(manager, "npm" | "pnpm" | "yarn" | "npx") {
            format!("{manager}.cmd")
        } else {
            manager.to_owned()
        };
        let arguments = if build {
            vec!["run".to_owned(), "build".to_owned()]
        } else {
            let mut arguments = vec![self.command.clone()];
            if matches!(self.command.as_str(), "run" | "run-script") {
                arguments.push(self.script.clone());
            }
            let script_arguments = util::shell::ShellKind::Posix
                .split(&self.arguments)
                .context("Invalid npm arguments: check for unmatched quotes")?;
            if !script_arguments.is_empty() {
                arguments.push("--".into());
                arguments.extend(script_arguments);
            }
            arguments
        };
        let mut environment = self.environment.clone();
        if !self.node_options.trim().is_empty() {
            let options = environment.entry("NODE_OPTIONS".into()).or_default();
            if !options.is_empty() {
                options.push(' ');
            }
            options.push_str(&self.node_options);
        }
        if let Some(runtime) = self
            .node_runtime
            .as_deref()
            .filter(|runtime| !runtime.trim().is_empty() && !runtime.starts_with("Project"))
        {
            let runtime = Path::new(runtime);
            ensure!(
                runtime.is_absolute(),
                "Choose an absolute path to the Node.js executable"
            );
            let runtime_directory = runtime
                .parent()
                .context("The Node.js executable has no parent directory")?;
            let path_key = environment
                .keys()
                .find(|key| {
                    if cfg!(windows) {
                        key.eq_ignore_ascii_case("PATH")
                    } else {
                        key.as_str() == "PATH"
                    }
                })
                .cloned()
                .unwrap_or_else(|| "PATH".into());
            let inherited_path = environment
                .get(&path_key)
                .map(std::ffi::OsString::from)
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default();
            let paths = std::iter::once(runtime_directory.to_path_buf())
                .chain(std::env::split_paths(&inherited_path));
            environment.insert(
                path_key,
                std::env::join_paths(paths)?.to_string_lossy().into_owned(),
            );
        }
        Ok(NpmLaunchCommand {
            command: manager,
            arguments,
            directory,
            environment,
        })
    }

    fn debug_scenario(&self, root: &Path, name: &str) -> Result<DebugScenario> {
        let NpmLaunchCommand {
            command,
            arguments,
            directory,
            environment,
        } = self.launch_command(root, false)?;
        Ok(DebugScenario {
            adapter: "JavaScript".into(),
            label: name.to_owned().into(),
            build: None,
            config: json!({
                "type": "pwa-node",
                "request": "launch",
                "runtimeExecutable": command,
                "runtimeArgs": arguments,
                "cwd": directory,
                "env": environment,
                "autoAttachChildProcesses": true,
                "sourceMaps": true
            }),
            tcp_connection: None,
        })
    }
}

struct NpmLaunchCommand {
    command: String,
    arguments: Vec<String>,
    directory: PathBuf,
    environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BrowserLiveEdit {
    pub launch_browser: bool,
    #[serde(default = "default_browser")]
    pub browser: String,
    pub url: String,
    pub start_debugger: bool,
}

fn default_browser() -> String {
    "Chrome".into()
}

fn deserialize_debug_target<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<DebugScenario>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut debug = Option::<DebugScenario>::deserialize(deserializer)?;
    if let Some(debug) = &mut debug
        && debug.config.get("request").and_then(Value::as_str) == Some("launch")
        && debug
            .config
            .get("program")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        && debug.config.get("targetCreateCommands").is_none()
        && let Some(task::BuildTaskDefinition::Template {
            task_template,
            locator_name,
        }) = &mut debug.build
        && locator_name.is_none()
        && task_template.command == "cargo"
        && task_template.args.first().is_some_and(|action| {
            matches!(
                action.as_str(),
                "build" | "b" | "run" | "r" | "test" | "t" | "bench"
            )
        })
    {
        // Older saved configurations omitted the locator, so restore Cargo
        // executable discovery unless an explicit target is configured.
        *locator_name = Some("rust-cargo-locator".into());
    }
    Ok(debug)
}

impl RunConfiguration {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.trim().is_empty(), "Enter a configuration name");
        if let Some(npm) = &self.npm {
            ensure!(!npm.package_json.trim().is_empty(), "Choose a package.json path");
            ensure!(!npm.command.trim().is_empty(), "Enter an npm command");
            if matches!(npm.command.as_str(), "run" | "run-script") {
                ensure!(
                    !npm.script.trim().is_empty(),
                    "Choose a script to run from package.json"
                );
            }
            return Ok(());
        }
        if let Some(type_id) = &self.configuration_type {
            if let Some(provider) = registry::ConfigurationProvider::find_by_id(type_id) {
                if !provider.supported {
                    if let Some(reason) = provider.unsupported_reason {
                        anyhow::bail!("{}: {reason}", provider.display_name);
                    }
                }
            }
        }
        ensure!(
            self.run.is_some() || self.debug.is_some() || self.configuration_type.as_deref() == Some("compound"),
            "Add a Run or Debug target"
        );
        if let Some(run) = &self.run {
            ensure!(
                !run.command.trim().is_empty(),
                "Enter a command for the Run target"
            );
        }
        if self.debug_enabled
            && let Some(debug) = &self.debug
        {
            ensure!(!debug.adapter.is_empty(), "Choose a debug adapter");
            ensure!(
                debug.config.is_object(),
                "Debug adapter options must be an object"
            );
            ensure!(
                matches!(
                    debug.config.get("request").and_then(Value::as_str),
                    Some("launch" | "attach")
                ),
                "Debug request must be launch or attach"
            );
        }
        Ok(())
    }

    pub fn kind(&self) -> &'static str {
        if let Some(type_id) = &self.configuration_type {
            return registry::ConfigurationProvider::display_name_for_type(type_id);
        }
        if self.npm.is_some() {
            return "npm";
        }
        match (
            &self.run,
            self.debug.as_ref().filter(|_| self.debug_enabled),
        ) {
            (Some(_), Some(_)) => "Run / Debug",
            (None, Some(debug))
                if debug.config.get("request").and_then(Value::as_str) == Some("attach") =>
            {
                "Attach"
            }
            (None, Some(_)) => "Debug",
            _ => "Run",
        }
    }

    pub fn icon(&self) -> IconName {
        if let Some(type_id) = &self.configuration_type {
            return registry::ConfigurationProvider::icon_for_type(type_id);
        }
        if self.npm.is_some() {
            return IconName::Terminal;
        }
        if self.debug_enabled && self.debug.is_some() && self.run.is_none() {
            return IconName::Debug;
        }
        IconName::PlayFilled
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Storage {
    Local,
    Project,
}

impl Storage {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Project => "Shared with project",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationKey {
    pub root: PathBuf,
    pub storage: Storage,
    pub id: String,
}

#[derive(Clone)]
pub struct ConfigurationEntry {
    pub key: ConfigurationKey,
    pub worktree_id: WorktreeId,
    pub configuration: RunConfiguration,
}

impl ConfigurationEntry {
    pub fn label(&self) -> String {
        let root_name = self
            .key
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        format!(
            "{} · {} · {} · {}",
            self.configuration.name,
            self.configuration.kind(),
            root_name,
            self.key.storage.label()
        )
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct LocalState {
    #[serde(default)]
    configurations: BTreeMap<String, RunConfiguration>,
    selected: Option<ConfigurationKey>,
}

#[derive(Deserialize)]
struct ConfigurationFile {
    version: u32,
    configurations: BTreeMap<String, RunConfiguration>,
}

fn parse_configuration_file(text: &str) -> Result<BTreeMap<String, RunConfiguration>> {
    let file: ConfigurationFile = settings_json::parse_json_with_comments(text)?;
    ensure!(
        file.version == 1,
        "Unsupported run configuration version: {}",
        file.version
    );
    ensure!(
        file.configurations.keys().all(|id| !id.is_empty()),
        "Configuration IDs must not be empty"
    );
    Ok(file.configurations)
}

fn configuration_file_edit(
    text: &str,
    id: &str,
    configuration: Option<&RunConfiguration>,
) -> Result<String> {
    let mut text = if text.is_empty() {
        "{\n  \"version\": 1,\n  \"configurations\": {}\n}\n".to_string()
    } else {
        text.to_string()
    };
    parse_configuration_file(&text)?;
    let old_document: Value = settings_json::parse_json_with_comments(&text)?;
    let mut new_document = old_document.clone();
    let configurations = new_document
        .get_mut("configurations")
        .and_then(Value::as_object_mut)
        .context("Expected configurations object")?;
    if let Some(configuration) = configuration {
        configuration.validate()?;
        let new_value = serde_json::to_value(configuration)?;
        let mut value = configurations.get(id).cloned().unwrap_or_else(|| json!({}));
        // Keep fields supplied by future versions or adapter tooling when editing a form.
        for key in [
            "name",
            "configuration_type",
            "debug_enabled",
            "folder",
            "allow_multiple_instances",
            "before_launch",
            "npm",
            "run",
            "debug",
        ] {
            if let Some(new_field) = new_value.get(key) {
                value[key] = new_field.clone();
            } else if let Some(object) = value.as_object_mut() {
                object.remove(key);
            }
        }
        configurations.insert(id.to_string(), value);
    } else {
        configurations.remove(id);
    }
    let indent = settings_json::infer_json_indent_size(&text);
    settings_json::update_value_in_json_text(
        &mut text,
        &mut Vec::new(),
        indent,
        &old_document,
        &new_document,
        &mut Vec::new(),
    );
    parse_configuration_file(&text)?;
    Ok(text)
}

fn persistence_key(root: &Path) -> String {
    format!("run-configurations-v1:{}", root.to_string_lossy())
}

pub struct RunConfigurations {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    fs: Arc<dyn Fs>,
    pub entries: Vec<ConfigurationEntry>,
    pub selected: Option<ConfigurationKey>,
    pub is_current_file_selected: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub roots: Vec<(WorktreeId, PathBuf)>,
    pub active_root: Option<PathBuf>,
    pub discovered_package_scripts: HashMap<PathBuf, BTreeMap<String, String>>,
    pub node_version: Option<String>,
    pub npm_version: Option<String>,
    pub templates: BTreeMap<String, RunConfiguration>,
    contexts: Arc<TaskContexts>,
    project_files: HashMap<PathBuf, String>,
    local_states: HashMap<PathBuf, LocalState>,
    refresh_task: Option<Shared<Task<()>>>,
    refresh_generation: usize,
    selection_save_task: Option<Task<()>>,
    subscriptions: Vec<Subscription>,
    task_configurations: HashMap<TaskId, (ConfigurationEntry, ResolvedTask)>,
    running_tasks: HashMap<TaskId, (ConfigurationKey, usize)>,
    debug_sessions: Vec<(
        ConfigurationKey,
        WeakEntity<project::debugger::session::Session>,
    )>,
}

impl RunConfigurations {
    pub fn new(
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let workspace_handle = workspace.weak_handle();
        let project = workspace.project().clone();
        let fs = workspace.app_state().fs.clone();
        cx.new(|cx: &mut Context<Self>| {
            let mut this = Self {
                workspace: workspace_handle.clone(),
                project,
                fs,
                entries: Vec::new(),
                selected: None,
                is_current_file_selected: false,
                loading: false,
                error: None,
                roots: Vec::new(),
                active_root: None,
                discovered_package_scripts: HashMap::default(),
                node_version: None,
                npm_version: None,
                templates: BTreeMap::default(),
                contexts: Arc::new(TaskContexts::default()),
                project_files: HashMap::default(),
                local_states: HashMap::default(),
                refresh_task: None,
                refresh_generation: 0,
                selection_save_task: None,
                subscriptions: Vec::new(),
                task_configurations: HashMap::default(),
                running_tasks: HashMap::default(),
                debug_sessions: Vec::new(),
            };

            #[cfg(any(test, feature = "test-support"))]
            {
                this.node_version = Some("22.23.3".into());
                this.npm_version = Some("10.9.9".into());
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                cx.spawn(async move |this, cx| {
                    let node_output = smol::process::Command::new("node")
                        .arg("--version")
                        .output()
                        .await
                        .ok();
                    let npm_output = smol::process::Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
                        .arg("--version")
                        .output()
                        .await
                        .ok();

                    let node_v = node_output.and_then(|o| {
                        if o.status.success() {
                            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            Some(s.strip_prefix('v').unwrap_or(&s).to_string())
                        } else {
                            None
                        }
                    });
                    let npm_v = npm_output.and_then(|o| {
                        if o.status.success() {
                            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                        } else {
                            None
                        }
                    });

                    let _ = cx.update(|cx| {
                        if let Some(this) = this.upgrade() {
                            this.update(cx, |this, cx| {
                                this.node_version = node_v;
                                this.npm_version = npm_v;
                                cx.notify();
                            });
                        }
                    });
                })
                .detach();
            }

            if let Some(workspace_handle) = workspace_handle.upgrade() {
                this.subscriptions.push(cx.subscribe_in(
                    &workspace_handle,
                    window,
                    |this, _, event: &workspace::Event, window, cx| match event {
                        workspace::Event::ActiveItemChanged => this.refresh(window, cx),
                        workspace::Event::TaskScheduled { task, omit_history } => {
                            if !omit_history {
                                this.task_scheduled(task, cx);
                            }
                        }
                        workspace::Event::TaskFinished(task_id) => {
                            if let Some((_, count)) = this.running_tasks.get_mut(task_id) {
                                *count -= 1;
                                if *count == 0 {
                                    this.running_tasks.remove(task_id);
                                }
                                cx.notify();
                            }
                        }
                        _ => {}
                    },
                ));
            }
            this.subscriptions.push(cx.subscribe_in(
                &this.project,
                window,
                |this, _, event: &project::Event, window, cx| {
                    let refresh = match event {
                        project::Event::WorktreeAdded(_) | project::Event::WorktreeRemoved(_) => {
                            true
                        }
                        project::Event::WorktreeUpdatedEntries(_, entries) => entries
                            .iter()
                            .any(|(path, _, _)| {
                                let unix = path.as_unix_str();
                                unix == CONFIGURATION_PATH || unix.ends_with("package.json")
                            }),
                        _ => false,
                    };
                    if refresh {
                        this.refresh(window, cx);
                    }
                },
            ));
            this.subscriptions.push(cx.on_release(|this, _| {
                if let Some(task) = this.selection_save_task.take() {
                    task.detach();
                }
            }));
            cx.defer_in(window, |this, window, cx| {
                this.refresh(window, cx);
            });
            this
        })
    }

    pub fn selected_entry(&self) -> Option<&ConfigurationEntry> {
        self.entries
            .iter()
            .chain(self.task_entries())
            .find(|entry| Some(&entry.key) == self.selected.as_ref())
    }

    pub fn task_entries(&self) -> impl Iterator<Item = &ConfigurationEntry> {
        self.task_configurations.values().map(|(entry, _)| entry)
    }

    fn task_scheduled(&mut self, task: &ResolvedTask, cx: &mut Context<Self>) {
        let configuration = self.entries.iter().find(|entry| {
            task.id.0.starts_with(&format!(
                "run-configuration:{}:{}_",
                entry.key.root.display(),
                entry.key.id
            ))
        });
        let key = if let Some(entry) = configuration {
            entry.key.clone()
        } else {
            let project = self.project.read(cx);
            let worktree = project
                .visible_worktrees(cx)
                .filter(|worktree| {
                    task.resolved
                        .cwd
                        .as_ref()
                        .is_some_and(|cwd| cwd.starts_with(worktree.read(cx).abs_path().as_ref()))
                })
                .max_by_key(|worktree| worktree.read(cx).abs_path().components().count())
                .or_else(|| project.visible_worktrees(cx).next());
            let Some(worktree) = worktree else {
                return;
            };
            let worktree = worktree.read(cx);
            let key = ConfigurationKey {
                root: worktree.abs_path().to_path_buf(),
                storage: Storage::Local,
                id: format!("task:{}", task.id.0),
            };
            let entry = ConfigurationEntry {
                key: key.clone(),
                worktree_id: worktree.id(),
                configuration: configuration_for_task(task),
            };
            self.task_configurations
                .insert(task.id.clone(), (entry, task.clone()));
            key
        };
        self.selected = Some(key.clone());
        self.is_current_file_selected = false;
        self.running_tasks
            .entry(task.id.clone())
            .or_insert((key, 0))
            .1 += 1;
        cx.notify();
    }

    pub fn can_stop_run(&self, cx: &App) -> bool {
        let has_task = self.running_tasks
            .values()
            .any(|(key, _)| Some(key) == self.selected.as_ref());
        if has_task {
            return true;
        }
        if let Some(key) = &self.selected {
            if let Some(workspace) = self.workspace.upgrade() {
                if let Some(panel) = workspace.read(cx).panel::<crate::run_panel::RunPanel>(cx) {
                    return panel.read(cx).is_running_configuration(key);
                }
            }
        }
        false
    }

    pub(crate) fn track_debug_session(
        &mut self,
        key: ConfigurationKey,
        session: &Entity<project::debugger::session::Session>,
        cx: &mut Context<Self>,
    ) {
        self.debug_sessions.push((key, session.downgrade()));
        self.subscriptions
            .push(cx.observe(session, |_, _, cx| cx.notify()));
        cx.notify();
    }

    pub fn can_stop_debug(&self, cx: &App) -> bool {
        self.debug_sessions.iter().any(|(key, session)| {
            Some(key) == self.selected.as_ref()
                && session
                    .upgrade()
                    .is_some_and(|session| !session.read(cx).is_terminated())
        })
    }

    fn stop_run(&self, cx: &mut Context<Self>) {
        let task_ids = self
            .running_tasks
            .iter()
            .filter(|(_, (key, _))| Some(key) == self.selected.as_ref())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        self.workspace
            .update(cx, |workspace, cx| {
                for task_id in task_ids {
                    workspace.stop_task(&task_id, cx);
                }
                if let Some(key) = &self.selected {
                    if let Some(panel) = workspace.panel::<crate::run_panel::RunPanel>(cx) {
                        panel.update(cx, |panel, cx| {
                            panel.stop_session_by_key(key, cx);
                        });
                    }
                }
            })
            .log_err();
    }

    fn stop_debug(&self, cx: &mut Context<Self>) {
        for (key, session) in &self.debug_sessions {
            if Some(key) == self.selected.as_ref()
                && let Some(session) = session.upgrade()
            {
                session.update(cx, |session, cx| session.shutdown(cx).detach());
            }
        }
    }

    pub fn selected_label(&self) -> String {
        if self.is_current_file_selected {
            return "Current File".into();
        }
        self.selected_entry()
            .map(|entry| entry.configuration.name.clone())
            .unwrap_or_else(|| "Select configuration".into())
    }

    pub fn selected_icon(&self) -> IconName {
        if self.is_current_file_selected {
            return IconName::Code;
        }
        self.selected_entry()
            .map(|entry| entry.configuration.icon())
            .unwrap_or(IconName::PlayFilled)
    }

    pub fn can_launch(&self, debug: bool, cx: &App) -> bool {
        if self.loading {
            return false;
        }
        if self.is_current_file_selected {
            return !debug;
        }
        self.selected_entry().is_some_and(|entry| {
            if entry.configuration.validate().is_err() {
                return false;
            }
            if debug && !entry.configuration.debug_enabled {
                return false;
            }
            if entry.configuration.npm.is_some() {
                return !debug || cx.global::<DapRegistry>().adapter("JavaScript").is_some();
            }
            if debug {
                entry.configuration.debug.as_ref().is_some_and(|debug| {
                    cx.global::<DapRegistry>().adapter(&debug.adapter).is_some()
                })
            } else {
                entry.configuration.run.is_some()
            }
        })
    }

    pub fn can_build(&self) -> bool {
        if self.loading {
            return false;
        }
        self.selected_entry().is_some_and(|entry| {
            if let Some(npm) = &entry.configuration.npm {
                let pkg_path = entry.key.root.join(&npm.package_json);
                if let Some(scripts) = self.discovered_package_scripts.get(&pkg_path) {
                    return scripts.contains_key("build");
                }
                return true;
            }
            entry
                .configuration
                .debug
                .as_ref()
                .and_then(|debug| debug.build.as_ref())
                .is_some()
        })
    }

    pub fn select(&mut self, key: ConfigurationKey, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        self.is_current_file_selected = false;
        if let Some(root) = self.active_root.clone() {
            self.local_states.entry(root.clone()).or_default().selected = Some(key.clone());
            let store = KeyValueStore::global(cx);
            let previous = self.selection_save_task.take();
            self.selection_save_task = Some(cx.spawn(async move |_, _| {
                if let Some(previous) = previous {
                    previous.await;
                }
                persist_selection(&store, &root, Some(key)).await.log_err();
            }));
        }
        cx.notify();
    }

    pub fn select_current_file(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.is_current_file_selected = true;
        cx.notify();
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok((contexts, roots, local_project, package_paths)) =
            self.workspace.update(cx, |workspace, cx| {
                let contexts = tasks_ui::task_contexts(workspace, window, cx);
                let roots = workspace
                    .visible_worktrees(cx)
                    .filter_map(|worktree| {
                        let worktree = worktree.read(cx);
                        worktree.root_entry().filter(|entry| entry.is_dir());
                        Some((worktree.id(), worktree.abs_path().to_path_buf()))
                    })
                    .collect::<Vec<_>>();
                let mut package_paths = Vec::new();
                for worktree in workspace.visible_worktrees(cx) {
                    let worktree = worktree.read(cx);
                    for entry in worktree.snapshot().files(false, 0) {
                        if entry.path.file_name() == Some("package.json")
                            && !entry
                                .path
                                .as_std_path()
                                .components()
                                .any(|component| component.as_os_str() == "node_modules")
                        {
                            package_paths.push(worktree.abs_path().join(entry.path.as_std_path()));
                        }
                    }
                }
                (
                    contexts,
                    roots,
                    workspace.project().read(cx).is_local(),
                    package_paths,
                )
            })
        else {
            return;
        };
        self.loading = true;
        self.refresh_generation = self.refresh_generation.wrapping_add(1);
        let generation = self.refresh_generation;
        let fs = self.fs.clone();
        let store = KeyValueStore::global(cx);
        let previous_save = self.selection_save_task.take();
        self.refresh_task = Some(
            cx.spawn_in(window, async move |this, cx| {
                if let Some(previous_save) = previous_save {
                    previous_save.await;
                }
                let contexts = contexts.await;
                let active_root = contexts
                    .worktree()
                    .and_then(|active| roots.iter().find(|(id, _)| *id == active))
                    .or_else(|| roots.first())
                    .map(|(_, root)| root.clone());
                let local_states = roots
                    .iter()
                    .map(|(_, root)| {
                        let state = if !local_project {
                            Ok(None)
                        } else {
                            store.read_kvp(&persistence_key(root)).and_then(|text| {
                                text.map(|text| {
                                    serde_json::from_str::<LocalState>(&text).map_err(Into::into)
                                })
                                .transpose()
                            })
                        };
                        (root.clone(), state)
                    })
                    .collect::<Vec<_>>();
                let root_paths = roots
                    .iter()
                    .map(|(_, root)| root.clone())
                    .collect::<Vec<_>>();

                // Load package.json scripts and configuration files
                let loaded = cx
                    .background_spawn(async move {
                        let mut files = Vec::new();
                        let mut discovered_scripts: HashMap<PathBuf, BTreeMap<String, String>> = HashMap::default();

                        for root in &root_paths {
                            let path = root.join(CONFIGURATION_PATH);
                            let result = if !local_project || fs.metadata(&path).await?.is_none() {
                                Ok(String::new())
                            } else {
                                fs.load(&path).await
                            };
                            files.push((root.clone(), result));
                        }
                        for package_path in package_paths {
                            let loaded = fs.load(&package_path).await.and_then(|text| {
                                let package: Value = serde_json::from_str(&text)?;
                                let scripts = package
                                    .get("scripts")
                                    .and_then(Value::as_object)
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|(name, command)| {
                                        Some((name.clone(), command.as_str()?.to_owned()))
                                    })
                                    .collect();
                                Ok(scripts)
                            });
                            match loaded {
                                Ok(scripts) => {
                                    discovered_scripts.insert(package_path, scripts);
                                }
                                Err(error) => log::warn!(
                                    "Could not load npm scripts from {}: {error:#}",
                                    package_path.display()
                                ),
                            }
                        }
                        anyhow::Ok((files, discovered_scripts))
                    })
                    .await;

                if let Err(error) = this.update(cx, |this, cx| {
                    if this.refresh_generation != generation {
                        return;
                    }
                    this.loading = false;
                    this.error = None;
                    this.entries.clear();
                    this.project_files.clear();
                    this.local_states.clear();
                    this.roots = roots;
                    this.active_root = active_root;
                    this.contexts = Arc::new(contexts);
                    for (root, state) in local_states {
                        match state {
                            Ok(state) => {
                                this.local_states.insert(root, state.unwrap_or_default());
                            }
                            Err(error) => {
                                this.error =
                                    Some(format!("Could not load local configurations: {error:#}"))
                            }
                        }
                    }
                    match loaded {
                        Ok((files, scripts)) => {
                            this.discovered_package_scripts = scripts;
                            for (root, text) in files {
                                match text.and_then(|text| {
                                    let configurations = if text.is_empty() {
                                        BTreeMap::new()
                                    } else {
                                        parse_configuration_file(&text)?
                                    };
                                    Ok((text, configurations))
                                }) {
                                    Ok((text, configurations)) => {
                                        this.project_files.insert(root.clone(), text);
                                        this.add_entries(&root, Storage::Project, configurations);
                                    }
                                    Err(error) => {
                                        this.error = Some(format!(
                                            "{}: {error:#}",
                                            root.join(CONFIGURATION_PATH).display()
                                        ))
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            this.error = Some(format!("Could not load configurations: {error:#}"))
                        }
                    }
                    for (root, state) in this.local_states.clone() {
                        this.add_entries(&root, Storage::Local, state.configurations);
                    }
                    this.entries.sort_by_key(|entry| entry.label());
                    let task_selection = this
                        .selected
                        .clone()
                        .filter(|key| this.task_entries().any(|entry| &entry.key == key));
                    this.selected = task_selection.or_else(|| {
                        this.active_root
                            .as_ref()
                            .and_then(|root| this.local_states.get(root))
                            .and_then(|state| state.selected.clone())
                    });
                    cx.notify();
                }) {
                    log::debug!("Run configuration workspace closed: {error:#}");
                }
            })
            .shared(),
        );
        cx.notify();
    }

    fn add_entries(
        &mut self,
        root: &Path,
        storage: Storage,
        configurations: BTreeMap<String, RunConfiguration>,
    ) {
        if let Some((worktree_id, _)) = self.roots.iter().find(|(_, path)| path == root) {
            self.entries
                .extend(
                    configurations
                        .into_iter()
                        .map(|(id, configuration)| ConfigurationEntry {
                            key: ConfigurationKey {
                                root: root.to_path_buf(),
                                storage: storage.clone(),
                                id,
                            },
                            worktree_id: *worktree_id,
                            configuration,
                        }),
                );
        }
    }

    pub fn launch_selected(&mut self, debug: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.launch_selected_inner(debug, false, window, cx);
    }

    fn launch_selected_inner(
        &mut self,
        debug: bool,
        build_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.selected.clone() else {
            return;
        };
        // Reload the file and editor context so a toolbar launch never uses stale settings.
        self.refresh(window, cx);
        let refresh = self.refresh_task.clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(refresh) = refresh {
                refresh.await;
            }
            wait_for_refresh(&this, cx).await?;
            let (entry, contexts, workspace, task) = this.read_with(cx, |this, _| {
                (
                    this.entries
                        .iter()
                        .chain(this.task_entries())
                        .find(|entry| entry.key == key)
                        .cloned(),
                    this.contexts.clone(),
                    this.workspace.clone(),
                    this.task_configurations
                        .values()
                        .find(|(entry, _)| entry.key == key)
                        .map(|(_, task)| task.clone()),
                )
            })?;
            let entry =
                entry.context("The selected configuration was removed or could not be loaded")?;
            entry.configuration.validate()?;
            ensure!(
                !debug || entry.configuration.debug_enabled,
                "Enable debugging in this configuration before starting a debug session"
            );
            if !build_only && !entry.configuration.before_launch.is_empty() {
                let entry = entry.clone();
                cx.background_spawn(async move { run_before_launch(&entry) })
                    .await?;
            }
            workspace.update_in(cx, |workspace, window, cx| {
                if !debug
                    && !build_only
                    && let Some(task) = task
                {
                    workspace.schedule_resolved_task(
                        TaskSourceKind::UserInput,
                        task,
                        false,
                        window,
                        cx,
                    );
                    return Ok(());
                }

                if build_only {
                    if let Some(npm) = &entry.configuration.npm {
                        let NpmLaunchCommand {
                            command,
                            arguments: args,
                            directory: cwd,
                            environment: env,
                        } = npm.launch_command(&entry.key.root, true)?;
                        workspace.open_panel::<crate::run_panel::RunPanel>(window, cx);
                        if let Some(panel) = workspace.panel::<crate::run_panel::RunPanel>(cx) {
                            panel.update(cx, |panel, cx| {
                                panel.launch(
                                    format!("Build {}", entry.configuration.name),
                                    IconName::Hammer,
                                    Some(entry.key.clone()),
                                    entry.configuration.clone(),
                                    command,
                                    args,
                                    cwd,
                                    env,
                                    window,
                                    cx,
                                );
                            });
                        }
                        return Ok(());
                    }

                    let build = entry
                        .configuration
                        .debug
                        .as_ref()
                        .and_then(|debug| debug.build.clone())
                        .context("This configuration does not have a Build task")?;
                    match build {
                        task::BuildTaskDefinition::ByName(name) => {
                            window.dispatch_action(
                                zed_actions::Spawn::ByName {
                                    task_name: name.to_string(),
                                    reveal_target: None,
                                }
                                .boxed_clone(),
                                cx,
                            );
                            Ok(())
                        }
                        task::BuildTaskDefinition::Template { task_template, .. } => {
                            let mut entry = entry.clone();
                            entry.configuration =
                                build_only_configuration(entry.configuration, task_template);
                            launch_configuration(workspace, &entry, &contexts, false, window, cx)
                        }
                    }
                } else {
                    launch_configuration(workspace, &entry, &contexts, debug, window, cx)
                }
            })??;
            Ok(())
        })
        .detach_and_prompt_err("Could not launch configuration", window, cx, |_, _, _| None);
    }

    pub fn save(
        &mut self,
        entry: ConfigurationEntry,
        original: Option<ConfigurationEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let validation = entry.configuration.validate();
        let local_project = self.project.read(cx).is_local();
        let expected = self.project_files.get(&entry.key.root).cloned();
        let selection_root = self.active_root.clone();
        let fs = self.fs.clone();
        let store = KeyValueStore::global(cx);
        let previous = self.selection_save_task.take();
        cx.spawn_in(window, async move |this, cx| {
            validation?;
            ensure!(local_project, "Run configurations currently require a local project");
            if let Some(previous) = previous { previous.await; }
            let mut local = read_local_state(&store, &entry.key.root)?;
            let expected = match expected {
                Some(expected) => expected,
                None => {
                    let path = entry.key.root.join(CONFIGURATION_PATH);
                    if fs.metadata(&path).await.ok().flatten().is_some() {
                        fs.load(&path).await.unwrap_or_default()
                    } else {
                        String::new()
                    }
                }
            };
            let shared = if expected.is_empty() { BTreeMap::new() } else { parse_configuration_file(&expected)? };
            if let Some(original) = &original {
                let current = match original.key.storage {
                    Storage::Local => local.configurations.get(&original.key.id),
                    Storage::Project => shared.get(&original.key.id),
                };
                ensure!(current == Some(&original.configuration), "This configuration changed outside this editor. Reopen Edit Configurations to reload it; your draft has not been saved");
            }
            let changing_storage = original.as_ref().is_some_and(|original| original.key.storage != entry.key.storage);
            match entry.key.storage {
                Storage::Project => {
                    let text = configuration_file_edit(&expected, &entry.key.id, Some(&entry.configuration))?;
                    write_configuration_file(fs.clone(), entry.key.root.clone(), expected, text).await?;
                    if changing_storage && let Some(original) = &original { local.configurations.remove(&original.key.id); }
                }
                Storage::Local => {
                    local.configurations.insert(entry.key.id.clone(), entry.configuration.clone());
                    store.write_kvp(persistence_key(&entry.key.root), serde_json::to_string(&local)?).await?;
                    if changing_storage && let Some(original) = &original {
                        let text = configuration_file_edit(&expected, &original.key.id, None)?;
                        write_configuration_file(fs.clone(), entry.key.root.clone(), expected, text).await?;
                    }
                }
            }
            local.selected = Some(entry.key.clone());
            store.write_kvp(persistence_key(&entry.key.root), serde_json::to_string(&local)?).await?;
            if let Some(root) = selection_root && root != entry.key.root {
                persist_selection(&store, &root, Some(entry.key.clone())).await?;
            }
            let refresh = this.update_in(cx, |this, window, cx| { this.refresh(window, cx); this.refresh_task.clone() })?;
            if let Some(refresh) = refresh { refresh.await; }
            wait_for_refresh(&this, cx).await?;
            Ok(())
        })
    }

    pub fn delete(
        &mut self,
        entry: ConfigurationEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let expected = self.project_files.get(&entry.key.root).cloned();
        let selection_root = self.active_root.clone();
        let fs = self.fs.clone();
        let store = KeyValueStore::global(cx);
        let previous = self.selection_save_task.take();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let mut local = read_local_state(&store, &entry.key.root)?;
            match entry.key.storage {
                Storage::Local => {
                    ensure!(
                        local.configurations.get(&entry.key.id) == Some(&entry.configuration),
                        "This local configuration changed outside this editor"
                    );
                    local.configurations.remove(&entry.key.id);
                }
                Storage::Project => {
                    let expected = match expected {
                        Some(expected) => expected,
                        None => {
                            let path = entry.key.root.join(CONFIGURATION_PATH);
                            if fs.metadata(&path).await.ok().flatten().is_some() {
                                fs.load(&path).await.unwrap_or_default()
                            } else {
                                String::new()
                            }
                        }
                    };
                    ensure!(
                        parse_configuration_file(&expected)?.get(&entry.key.id)
                            == Some(&entry.configuration),
                        "This configuration changed outside this editor"
                    );
                    let text = configuration_file_edit(&expected, &entry.key.id, None)?;
                    write_configuration_file(fs, entry.key.root.clone(), expected, text).await?;
                }
            }
            if local.selected.as_ref() == Some(&entry.key) {
                local.selected = None;
            }
            store
                .write_kvp(
                    persistence_key(&entry.key.root),
                    serde_json::to_string(&local)?,
                )
                .await?;
            if let Some(root) = selection_root
                && root != entry.key.root
            {
                let state = read_local_state(&store, &root)?;
                if state.selected.as_ref() == Some(&entry.key) {
                    persist_selection(&store, &root, None).await?;
                }
            }
            let refresh = this.update_in(cx, |this, window, cx| {
                this.refresh(window, cx);
                this.refresh_task.clone()
            })?;
            if let Some(refresh) = refresh {
                refresh.await;
            }
            wait_for_refresh(&this, cx).await?;
            Ok(())
        })
    }
}

async fn wait_for_refresh(
    this: &WeakEntity<RunConfigurations>,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<()> {
    loop {
        let refresh = this.read_with(cx, |this, _| {
            if this.loading {
                this.refresh_task.clone()
            } else {
                None
            }
        })?;
        let Some(refresh) = refresh else {
            return Ok(());
        };
        refresh.await;
    }
}

fn read_local_state(store: &KeyValueStore, root: &Path) -> Result<LocalState> {
    store
        .read_kvp(&persistence_key(root))?
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map(|state| state.unwrap_or_default())
        .map_err(Into::into)
}

async fn persist_selection(
    store: &KeyValueStore,
    root: &Path,
    selected: Option<ConfigurationKey>,
) -> Result<()> {
    let mut state = read_local_state(store, root)?;
    state.selected = selected;
    store
        .write_kvp(persistence_key(root), serde_json::to_string(&state)?)
        .await
}

async fn write_configuration_file(
    fs: Arc<dyn Fs>,
    root: PathBuf,
    expected: String,
    text: String,
) -> Result<()> {
    let path = root.join(CONFIGURATION_PATH);
    let current = if fs.metadata(&path).await?.is_some() {
        fs.load(&path).await?
    } else {
        String::new()
    };
    ensure!(
        current == expected,
        "run.json changed outside this editor. Reopen Edit Configurations to reload it; your draft has not been saved"
    );
    fs.create_dir(path.parent().context("Missing configuration directory")?)
        .await?;
    fs.atomic_write(path, text).await
}

fn run_before_launch(entry: &ConfigurationEntry) -> Result<()> {
    if !entry.configuration.before_launch.is_empty() {
        for task in &entry.configuration.before_launch {
            match task {
                BeforeLaunchTask::NpmScript {
                    script,
                    package_json,
                } => {
                    let pkg_str = package_json
                        .as_deref()
                        .or_else(|| {
                            entry
                                .configuration
                                .npm
                                .as_ref()
                                .map(|npm| npm.package_json.as_str())
                        })
                        .unwrap_or("package.json");
                    let pkg_path = entry.key.root.join(pkg_str);
                    let cwd = pkg_path.parent().unwrap_or(&entry.key.root);
                    let mut npm = entry.configuration.npm.clone().unwrap_or_default();
                    npm.package_json = pkg_str.to_owned();
                    npm.command = "run".into();
                    npm.script = script.clone();
                    npm.arguments.clear();
                    let NpmLaunchCommand {
                        command,
                        arguments,
                        environment,
                        ..
                    } = npm.launch_command(&entry.key.root, false)?;
                    let output = task::ShellBuilder::new(&task::Shell::System, cfg!(windows))
                        .non_interactive()
                        .build_std_command(Some(command), &arguments)
                        .envs(environment)
                        .current_dir(cwd)
                        .output()
                        .context(format!(
                            "Failed to execute before-launch task: npm run {script}"
                        ))?;
                    if !output.status.success() {
                        anyhow::bail!(
                            "Before launch task 'npm run {script}' failed:\n{}\n{}",
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                }
                BeforeLaunchTask::Build { name, target } => {
                    let mut cmd = util::command::new_std_command("cargo");
                    cmd.arg("build");
                    if let Some(target) = target {
                        cmd.args(["--bin", target]);
                    }
                    cmd.current_dir(&entry.key.root);
                    let output = cmd.output().context(format!(
                        "Failed to execute before-launch build task: {name}"
                    ))?;
                    if !output.status.success() {
                        let err_text = String::from_utf8_lossy(&output.stderr);
                        anyhow::bail!("Before launch task '{name}' failed:\n{err_text}");
                    }
                }
                BeforeLaunchTask::RunConfiguration { configuration_id } => anyhow::bail!(
                    "Before-launch configuration '{configuration_id}' is not supported yet"
                ),
            }
        }
    }
    Ok(())
}

fn launch_configuration(
    workspace: &mut Workspace,
    entry: &ConfigurationEntry,
    contexts: &TaskContexts,
    debug: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    entry.configuration.validate()?;
    ensure!(
        !debug || entry.configuration.debug_enabled,
        "Enable debugging in this configuration before starting a debug session"
    );
    let context = if contexts.worktree() == Some(entry.worktree_id) {
        contexts.active_context()
    } else {
        contexts.task_context_for_worktree_id(entry.worktree_id)
    }
    .context("The configuration's project root is no longer open")?;

    if debug {
        let scenario = if let Some(npm) = &entry.configuration.npm {
            npm.debug_scenario(&entry.key.root, &entry.configuration.name)?
        } else {
            entry
                .configuration
                .debug
                .clone()
                .context("This configuration does not have a Debug target")?
        };

        ensure!(
            DapRegistry::global(cx).adapter(&scenario.adapter).is_some(),
            "The {} debug adapter is unavailable",
            scenario.adapter
        );
        let panel = workspace
            .panel::<DebugPanel>(cx)
            .context("The debugger panel is unavailable")?;
        let buffer = contexts
            .location()
            .filter(|location| {
                location
                    .buffer
                    .read(cx)
                    .file()
                    .is_some_and(|file| file.worktree_id(cx) == entry.worktree_id)
            })
            .map(|location| location.buffer.clone());
        panel.update(cx, |panel, cx| {
            panel.start_configuration_session(
                scenario,
                context.clone().into(),
                buffer,
                Some(entry.worktree_id),
                Some(entry.key.clone()),
                window,
                cx,
            )
        });
    } else if let Some(npm) = &entry.configuration.npm {
        let NpmLaunchCommand {
            command,
            arguments: args,
            directory: cwd,
            environment: env,
        } = npm.launch_command(&entry.key.root, false)?;
        workspace.open_panel::<crate::run_panel::RunPanel>(window, cx);
        if let Some(panel) = workspace.panel::<crate::run_panel::RunPanel>(cx) {
            panel.update(cx, |panel, cx| {
                panel.launch(
                    entry.configuration.name.clone(),
                    entry.configuration.icon(),
                    Some(entry.key.clone()),
                    entry.configuration.clone(),
                    command,
                    args,
                    cwd,
                    env,
                    window,
                    cx,
                );
            });
        }
    } else {
        let mut task = entry
            .configuration
            .run
            .clone()
            .context("This configuration does not have a Run target")?;
        task.label = entry.configuration.name.clone();
        let source = TaskSourceKind::UserInput;
        let mut resolved = task
            .resolve_task(
                &format!(
                    "run-configuration:{}:{}",
                    entry.key.root.display(),
                    entry.key.id
                ),
                context,
            )
            .context("Could not resolve the configuration's task variables")?;

        // Try RunPanel first, otherwise fallback to terminal provider
        let run_panel = workspace.panel::<crate::run_panel::RunPanel>(cx);
        if let Some(panel) = run_panel {
            workspace.open_panel::<crate::run_panel::RunPanel>(window, cx);
            let command = resolved.resolved.command.clone().unwrap_or_default();
            let args = resolved.resolved.args.clone();
            let cwd = resolved.resolved.cwd.clone().unwrap_or_else(|| entry.key.root.clone());
            let env = resolved.resolved.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            panel.update(cx, |panel, cx| {
                panel.launch(
                    entry.configuration.name.clone(),
                    entry.configuration.icon(),
                    Some(entry.key.clone()),
                    entry.configuration.clone(),
                    command,
                    args,
                    cwd,
                    env,
                    window,
                    cx,
                );
            });
        } else {
            let shell = task::ShellBuilder::new(&resolved.resolved.shell, cfg!(windows)).kind();
            for argument in &mut resolved.resolved.args {
                *argument = shell
                    .try_quote(argument)
                    .context("Could not quote a configuration argument for the selected shell")?
                    .into_owned();
            }
            workspace.schedule_resolved_task(source, resolved, false, window, cx);
        }
    }
    Ok(())
}

fn build_only_configuration(
    mut configuration: RunConfiguration,
    task: TaskTemplate,
) -> RunConfiguration {
    configuration.name = format!("Build {}", configuration.name);
    configuration.run = Some(task);
    configuration.debug = None;
    configuration.debug_enabled = false;
    configuration
}

fn configuration_for_task(task: &ResolvedTask) -> RunConfiguration {
    let runtime = task
        .resolved
        .command
        .as_deref()
        .and_then(|command| Path::new(command).file_stem())
        .and_then(|name| name.to_str());
    let debug = matches!(
        runtime,
        Some("node" | "bun" | "npm" | "npx" | "pnpm" | "yarn")
    )
    .then(|| DebugScenario {
        adapter: "JavaScript".into(),
        label: task.resolved_label.clone().into(),
        build: None,
        config: json!({
            "type": "node-terminal",
            "request": "launch",
            "command": task.resolved.command_label,
            "cwd": task.resolved.cwd,
            "env": task.resolved.env,
        }),
        tcp_connection: None,
    });
    RunConfiguration {
        debug_enabled: false,
        name: task.resolved_label.clone(),
        configuration_type: Some("task".into()),
        folder: None,
        allow_multiple_instances: false,
        before_launch: Vec::new(),
        npm: None,
        run: Some(task.original_task().clone()),
        debug,
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace
            .register_action(|workspace, _: &EditConfigurations, window, cx| {
                ConfigurationEditor::show(workspace, window, cx);
            })
            .register_action(|workspace, _: &RunSelected, window, cx| {
                launch_selected_action(workspace, false, false, window, cx);
            })
            .register_action(|workspace, _: &BuildSelected, window, cx| {
                launch_selected_action(workspace, false, true, window, cx);
            })
            .register_action(|workspace, _: &DebugSelected, window, cx| {
                launch_selected_action(workspace, true, false, window, cx);
            })
            .register_action(|workspace, _: &StopSelectedRun, window, cx| {
                stop_selected_action(workspace, false, window, cx);
            })
            .register_action(|workspace, _: &StopSelectedDebug, window, cx| {
                stop_selected_action(workspace, true, window, cx);
            })
            .register_action(|workspace, _: &StopAllProcesses, _window, cx| {
                if let Some(panel) = workspace.panel::<crate::run_panel::RunPanel>(cx) {
                    panel.update(cx, |panel, cx| {
                        panel.stop_all(cx);
                    });
                }
            })
            .register_action(|workspace, _: &RunNpmTask, window, cx| {
                ConfigurationEditor::show(workspace, window, cx);
            })
            .register_action(|workspace, _: &ManageTargets, window, cx| {
                ConfigurationEditor::show(workspace, window, cx);
            })
            .register_action(|workspace, _: &ShowRunningList, window, cx| {
                workspace.open_panel::<crate::run_panel::RunPanel>(window, cx);
            })
            .register_action(|workspace, _: &RunPrompt, window, cx| {
                ConfigurationEditor::show(workspace, window, cx);
            })
            .register_action(|workspace, _: &DebugPrompt, window, cx| {
                ConfigurationEditor::show(workspace, window, cx);
            })
            .register_action(|_workspace, _: &RunWithCoverage, _, _| {})
            .register_action(|_workspace, _: &Profile, _, _| {});
    })
    .detach();
}

fn stop_selected_action(
    workspace: &Workspace,
    debug: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(panel) = workspace.panel::<DebugPanel>(cx) else {
        return;
    };
    let configurations = panel.read(cx).run_configurations.clone();
    let active_session = panel.read(cx).active_session();
    cx.spawn_in(window, async move |_, cx| {
        configurations.update(cx, |configurations, cx| {
            if debug {
                if configurations.selected.is_none() {
                    if let Some(session) = active_session {
                        session
                            .read(cx)
                            .session(cx)
                            .update(cx, |session, cx| session.shutdown(cx).detach());
                    }
                } else {
                    configurations.stop_debug(cx);
                }
            } else {
                configurations.stop_run(cx);
            }
        });
    })
    .detach();
}

fn launch_selected_action(
    workspace: &Workspace,
    debug: bool,
    build_only: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(panel) = workspace.panel::<DebugPanel>(cx) {
        let configurations = panel.read(cx).run_configurations.clone();
        cx.spawn_in(window, async move |_, cx| {
            configurations
                .update_in(cx, |configurations, window, cx| {
                    configurations.launch_selected_inner(debug, build_only, window, cx)
                })
                .log_err();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debugger_enablement_defaults_off_and_persists_both_states() -> Result<()> {
        for kind in ["npm", "node", "javascript_debug", "attach_node", "cargo"] {
            let mut configuration =
                registry::ConfigurationProvider::create_default(kind, Path::new("/project"), None);
            assert!(
                !configuration.debug_enabled,
                "{kind} must default to disabled"
            );
            let original_debug = configuration.debug.clone();
            let disabled = serde_json::to_string(&configuration)?;
            assert!(!serde_json::from_str::<RunConfiguration>(&disabled)?.debug_enabled);
            configuration.debug_enabled = true;
            let enabled = configuration_file_edit("", "configuration", Some(&configuration))?;
            assert!(
                parse_configuration_file(&enabled)?
                    .get("configuration")
                    .context("Missing enabled configuration")?
                    .debug_enabled
            );
            configuration.debug_enabled = false;
            let disabled =
                configuration_file_edit(&enabled, "configuration", Some(&configuration))?;
            let restored = parse_configuration_file(&disabled)?;
            let restored = restored
                .get("configuration")
                .context("Missing disabled configuration")?;
            assert!(!restored.debug_enabled);
            assert_eq!(restored.debug, original_debug);
        }
        Ok(())
    }

    #[gpui::test]
    async fn debugger_toggle_controls_launch_availability(cx: &mut gpui::TestAppContext) {
        let (_, _, store) = test_workspace(cx).await;
        store.update(cx, |store, cx| {
            let (worktree_id, root) = store.roots.first().expect("Project root").clone();
            let mut entry = ConfigurationEntry {
                key: ConfigurationKey {
                    root: root.clone(),
                    storage: Storage::Project,
                    id: "toggle".into(),
                },
                worktree_id,
                configuration: registry::ConfigurationProvider::create_default("npm", &root, None),
            };
            store.selected = Some(entry.key.clone());
            store.is_current_file_selected = false;
            store.loading = false;
            store.entries = vec![entry.clone()];
            assert!(store.can_launch(false, cx));
            assert!(!store.can_launch(true, cx));
            entry.configuration.debug_enabled = true;
            store.entries = vec![entry.clone()];
            assert!(store.can_launch(true, cx));
            entry.configuration.debug_enabled = false;
            store.entries = vec![entry];
            assert!(!store.can_launch(true, cx));
            assert!(store.can_launch(false, cx));
        });
    }

    #[test]
    fn npm_debug_uses_the_same_command_and_environment_as_run() -> Result<()> {
        let root = std::env::current_dir()?;
        let runtime =
            root.join("node runtime")
                .join(if cfg!(windows) { "node.exe" } else { "node" });
        let npm = NpmConfigurationFields {
            package_json: "backend/package.json".into(),
            command: "run-script".into(),
            script: "start:dev".into(),
            arguments: "--label \"two words\" --port 3000".into(),
            node_runtime: Some(runtime.to_string_lossy().into_owned()),
            node_options: "--enable-source-maps".into(),
            environment: [("NODE_OPTIONS".into(), "--max-old-space-size=4096".into())].into(),
            ..Default::default()
        };
        let NpmLaunchCommand {
            command: manager,
            arguments,
            directory,
            environment,
        } = npm.launch_command(&root, false)?;
        assert_eq!(
            arguments,
            [
                "run-script",
                "start:dev",
                "--",
                "--label",
                "two words",
                "--port",
                "3000"
            ]
        );
        assert_eq!(directory, root.join("backend"));
        assert_eq!(
            environment.get("NODE_OPTIONS").map(String::as_str),
            Some("--max-old-space-size=4096 --enable-source-maps")
        );
        let path = environment.get("PATH").context("Missing runtime PATH")?;
        assert_eq!(
            std::env::split_paths(path).next().as_deref(),
            runtime.parent()
        );
        let debug = npm.debug_scenario(&root, "Backend")?;
        assert_eq!(debug.config["runtimeExecutable"], json!(manager));
        assert_eq!(debug.config["runtimeArgs"], json!(arguments));
        assert_eq!(debug.config["env"], json!(environment));
        assert_eq!(debug.config["autoAttachChildProcesses"], true);
        Ok(())
    }

    #[test]
    fn npm_commands_do_not_duplicate_lifecycle_script_names() -> Result<()> {
        let mut npm = NpmConfigurationFields {
            package_json: "package.json".into(),
            command: "start".into(),
            script: "start".into(),
            package_manager: Some("Project npm, version 10.9.9".into()),
            ..Default::default()
        };
        let NpmLaunchCommand {
            command: manager,
            arguments,
            ..
        } = npm.launch_command(Path::new("."), false)?;
        assert_eq!(manager, if cfg!(windows) { "npm.cmd" } else { "npm" });
        assert_eq!(arguments, ["start"]);
        npm.arguments = "\"unclosed".into();
        assert!(npm.launch_command(Path::new("."), false).is_err());
        let NpmLaunchCommand { arguments, .. } = npm.launch_command(Path::new("."), true)?;
        assert_eq!(arguments, ["run", "build"]);
        Ok(())
    }

    #[test]
    fn saved_cargo_debug_configuration_restores_executable_discovery() -> Result<()> {
        let mut configuration = configuration("Cargo application");
        configuration.debug = Some(DebugScenario {
            adapter: "CodeLLDB".into(),
            label: "Cargo application".into(),
            build: Some(task::BuildTaskDefinition::Template {
                task_template: TaskTemplate {
                    label: "Build".into(),
                    command: "cargo".into(),
                    args: vec!["build".into()],
                    ..TaskTemplate::default()
                },
                locator_name: Some("rust-cargo-locator".into()),
            }),
            config: json!({"request": "launch"}),
            tcp_connection: None,
        });
        let saved = serde_json::to_string(&configuration)?;
        let reopened: RunConfiguration = serde_json::from_str(&saved)?;
        assert_eq!(reopened, configuration);

        let mut legacy = serde_json::to_value(&configuration)?;
        legacy["debug"]["build"]
            .as_object_mut()
            .context("Expected a build task object")?
            .remove("locator_name");
        let reopened: RunConfiguration = serde_json::from_value(legacy.clone())?;
        assert_eq!(reopened, configuration);

        legacy["debug"]["targetCreateCommands"] = json!(["target create /custom/program"]);
        let reopened: RunConfiguration = serde_json::from_value(legacy)?;
        assert!(matches!(
            reopened.debug.and_then(|debug| debug.build),
            Some(task::BuildTaskDefinition::Template {
                locator_name: None,
                ..
            })
        ));

        let debug = configuration
            .debug
            .as_mut()
            .context("Missing debug target")?;
        debug.config["program"] = json!("/custom/program");
        if let Some(task::BuildTaskDefinition::Template { locator_name, .. }) = &mut debug.build {
            *locator_name = None;
        }
        let reopened: RunConfiguration =
            serde_json::from_str(&serde_json::to_string(&configuration)?)?;
        assert_eq!(reopened, configuration);
        Ok(())
    }

    #[test]
    fn build_only_uses_build_task_instead_of_run_target() {
        let original = configuration("Application");
        let built = build_only_configuration(
            original.clone(),
            TaskTemplate {
                label: "Build".into(),
                command: "cargo".into(),
                args: vec!["build".into()],
                ..TaskTemplate::default()
            },
        );
        assert_eq!(
            built.run.as_ref().map(|task| task.command.as_str()),
            Some("cargo")
        );
        assert_eq!(
            built.run.as_ref().map(|task| task.args.as_slice()),
            Some(["build".to_owned()].as_slice())
        );
        assert!(built.debug.is_none());
        assert_eq!(
            original.run.as_ref().map(|task| task.command.as_str()),
            Some("echo")
        );
    }

    fn configuration(name: &str) -> RunConfiguration {
        RunConfiguration {
            debug_enabled: false,
            name: name.into(),
            configuration_type: Some("cargo".into()),
            folder: None,
            allow_multiple_instances: false,
            before_launch: Vec::new(),
            npm: None,
            run: Some(TaskTemplate {
                label: name.into(),
                command: "echo".into(),
                ..TaskTemplate::default()
            }),
            debug: None,
        }
    }

    #[test]
    fn configuration_edits_preserve_comments_and_ids() -> Result<()> {
        let text = r#"{
            // Shared configurations
            "version": 1,
            "configurations": {
                "stable-id": {
                    // Keep this name comment
                    "name": "Old name",
                    "future_field": true,
                    "run": { "label": "Old name", "command": "echo" }
                }
            },
            "future_metadata": { "enabled": true }
        }"#;
        let updated = configuration_file_edit(text, "stable-id", Some(&configuration("New name")))?;
        assert!(updated.contains("// Shared configurations"));
        assert!(updated.contains("// Keep this name comment"));
        let document: Value = settings_json::parse_json_with_comments(&updated)?;
        assert_eq!(document["configurations"]["stable-id"]["name"], "New name");
        assert_eq!(
            document["configurations"]["stable-id"]["future_field"],
            true
        );
        assert_eq!(document["future_metadata"]["enabled"], true);
        Ok(())
    }

    #[test]
    fn create_and_delete_configuration_without_changing_siblings() -> Result<()> {
        let text = configuration_file_edit("", "first", Some(&configuration("Same name")))?;
        let text = configuration_file_edit(&text, "second", Some(&configuration("Same name")))?;
        assert_eq!(parse_configuration_file(&text)?.len(), 2);
        let text = configuration_file_edit(&text, "first", None)?;
        let configurations = parse_configuration_file(&text)?;
        assert_eq!(configurations.len(), 1);
        assert!(configurations.contains_key("second"));
        Ok(())
    }

    #[test]
    fn rejects_unsupported_version_and_invalid_targets() {
        assert!(parse_configuration_file(r#"{"version":2,"configurations":{}}"#).is_err());
        assert!(
            RunConfiguration {
                debug_enabled: false,
                name: "Empty".into(),
                configuration_type: None,
                folder: None,
                allow_multiple_instances: false,
                before_launch: Vec::new(),
                npm: None,
                run: None,
                debug: None
            }
            .validate()
            .is_err()
        );
        assert!(configuration(" ").validate().is_err());
    }

    #[test]
    fn test_npm_configuration_serialization_and_validation() -> Result<()> {
        let npm_config = RunConfiguration {
            debug_enabled: false,
            name: "Backend".into(),
            configuration_type: Some("npm".into()),
            folder: None,
            allow_multiple_instances: false,
            before_launch: vec![BeforeLaunchTask::NpmScript {
                script: "build".into(),
                package_json: Some("package.json".into()),
            }],
            npm: Some(NpmConfigurationFields {
                package_json: "package.json".into(),
                command: "run".into(),
                script: "start:luke_dev".into(),
                arguments: "--port 3000".into(),
                node_runtime: Some("Project node, version 22.23.3".into()),
                node_options: "--inspect".into(),
                package_manager: Some("Project npm, version 10.9.9".into()),
                environment: [("PORT".into(), "3000".into())].into(),
                browser_live_edit: Some(BrowserLiveEdit {
                    launch_browser: true,
                    browser: "Chrome".into(),
                    url: "http://localhost:3000".into(),
                    start_debugger: true,
                }),
            }),
            run: None,
            debug: None,
        };

        npm_config.validate()?;
        assert_eq!(npm_config.kind(), "npm");

        let serialized = serde_json::to_string_pretty(&npm_config)?;
        let deserialized: RunConfiguration = serde_json::from_str(&serialized)?;
        assert_eq!(npm_config, deserialized);
        Ok(())
    }

    async fn test_workspace(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Arc<fs::FakeFs>,
        gpui::WindowHandle<workspace::MultiWorkspace>,
        Entity<RunConfigurations>,
    ) {
        crate::tests::init_test(cx);
        let fs = fs::FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(Path::new("/run-first"), json!({"main.rs": "fn main() {}"}))
            .await;
        fs.insert_tree(Path::new("/run-second"), json!({"main.rs": "fn main() {}"}))
            .await;
        let project = Project::test(
            fs.clone(),
            [Path::new("/run-first"), Path::new("/run-second")],
            cx,
        )
        .await;
        let window = crate::tests::init_test_workspace(&project, cx).await;
        cx.run_until_parked();
        let configurations = window
            .update(cx, |multi, _, cx| {
                multi
                    .workspace()
                    .read(cx)
                    .panel::<DebugPanel>(cx)
                    .expect("debug panel")
                    .read(cx)
                    .run_configurations
                    .clone()
            })
            .expect("workspace window");
        (fs, window, configurations)
    }

    #[gpui::test]
    async fn npm_scripts_are_discovered_in_nested_project_packages(cx: &mut gpui::TestAppContext) {
        let (fs, window, store) = test_workspace(cx).await;
        fs.insert_tree(Path::new("/run-second"), json!({
            "backend": { "package.json": r#"{"scripts":{"start:luke_dev":"node server.js","start:luke_dev_fake_data":"node fake.js"}}"# },
            "apps": { "frontend": { "package.json": r#"{"scripts":{"start:dev":"vite"}}"# } },
            "node_modules": { "dependency": { "package.json": r#"{"scripts":{"unrelated":"echo dependency"}}"# } }
        })).await;
        cx.run_until_parked();
        window
            .update(cx, |_, window, cx| {
                store.update(cx, |store, cx| store.refresh(window, cx));
            })
            .expect("Refresh npm package scripts");
        cx.run_until_parked();
        store
            .read_with(cx, |store, _| {
                let backend = store
                    .discovered_package_scripts
                    .get(Path::new("/run-second/backend/package.json"))
                    .context("Backend package scripts were not discovered")?;
                assert!(backend.contains_key("start:luke_dev"));
                assert!(backend.contains_key("start:luke_dev_fake_data"));
                let frontend = store
                    .discovered_package_scripts
                    .get(Path::new("/run-second/apps/frontend/package.json"))
                    .context("Nested frontend package scripts were not discovered")?;
                assert!(frontend.contains_key("start:dev"));
                assert!(!store.discovered_package_scripts.contains_key(Path::new(
                    "/run-second/node_modules/dependency/package.json"
                )));
                anyhow::Ok(())
            })
            .expect("Discover scripts in nested packages");
    }

    #[gpui::test]
    async fn test_run_configuration_save_rename_and_storage_move(cx: &mut gpui::TestAppContext) {
        let (fs, window, store) = test_workspace(cx).await;
        let worktree_id = store
            .read_with(cx, |store, _| {
                store
                    .roots
                    .iter()
                    .find(|(_, root)| root == Path::new("/run-second"))
                    .map(|(id, _)| *id)
            })
            .expect("second root");
        let mut entry = ConfigurationEntry {
            key: ConfigurationKey {
                root: PathBuf::from("/run-second"),
                storage: Storage::Project,
                id: "stable-id".into(),
            },
            worktree_id,
            configuration: configuration("Server"),
        };
        window
            .update(cx, |_, window, cx| {
                store.update(cx, |store, cx| store.save(entry.clone(), None, window, cx))
            })
            .expect("window")
            .await
            .expect("save project configuration");
        store.read_with(cx, |store, _| {
            assert_eq!(store.selected.as_ref(), Some(&entry.key))
        });
        let original = entry.clone();
        entry.configuration.name = "Renamed server".into();
        window
            .update(cx, |_, window, cx| {
                store.update(cx, |store, cx| {
                    store.save(entry.clone(), Some(original), window, cx)
                })
            })
            .expect("window")
            .await
            .expect("rename configuration");
        let contents = fs
            .load(&entry.key.root.join(CONFIGURATION_PATH))
            .await
            .expect("load file");
        assert!(contents.contains("Renamed server"));
    }

    #[test]
    fn test_npm_configuration_persistence_across_restart() -> Result<()> {
        let original_file = r#"{
            "version": 1,
            "configurations": {}
        }"#;

        let npm_config = RunConfiguration {
            debug_enabled: false,
            name: "Backend".into(),
            configuration_type: Some("npm".into()),
            folder: Some("Services".into()),
            allow_multiple_instances: false,
            before_launch: vec![
                BeforeLaunchTask::NpmScript {
                    script: "build".into(),
                    package_json: Some("package.json".into()),
                },
            ],
            npm: Some(NpmConfigurationFields {
                package_json: "package.json".into(),
                command: "run".into(),
                script: "start:luke_dev".into(),
                arguments: "--inspect".into(),
                node_runtime: Some("Project node, version 22.23.3".into()),
                node_options: "--max-old-space-size=4096".into(),
                package_manager: Some("Project npm, version 10.9.9".into()),
                environment: [("NODE_ENV".into(), "development".into())].into(),
                browser_live_edit: Some(BrowserLiveEdit {
                    launch_browser: true,
                    browser: "Chrome".into(),
                    url: "http://localhost:3000".into(),
                    start_debugger: true,
                }),
            }),
            run: None,
            debug: None,
        };

        // 1. Edit/save configuration to .zed/run.json text
        let updated_file = configuration_file_edit(original_file, "backend-id", Some(&npm_config))?;
        assert!(updated_file.contains("Backend"));
        assert!(updated_file.contains("start:luke_dev"));
        assert!(updated_file.contains("Services"));

        // 2. Simulate restart: parse file anew
        let parsed = parse_configuration_file(&updated_file)?;
        assert_eq!(parsed.len(), 1);
        let restored = parsed.get("backend-id").expect("saved configuration");
        assert_eq!(restored.name, "Backend");
        assert_eq!(restored.folder.as_deref(), Some("Services"));
        assert_eq!(restored.allow_multiple_instances, false);
        assert_eq!(restored.before_launch.len(), 1);

        let restored_npm = restored.npm.as_ref().expect("npm configuration");
        assert_eq!(restored_npm.command, "run");
        assert_eq!(restored_npm.script, "start:luke_dev");
        assert_eq!(restored_npm.node_options, "--max-old-space-size=4096");
        assert_eq!(restored_npm.environment.get("NODE_ENV").map(String::as_str), Some("development"));

        let browser = restored_npm.browser_live_edit.as_ref().expect("browser live edit");
        assert_eq!(browser.launch_browser, true);
        assert_eq!(browser.browser, "Chrome");
        assert_eq!(browser.url, "http://localhost:3000");
        assert_eq!(browser.start_debugger, true);

        Ok(())
    }

    #[test]
    fn test_provider_registry_compatibility() {
        use registry::*;

        // All requested WebStorm types must be in registry
        let providers = ConfigurationProvider::all();
        assert!(providers.iter().any(|p| p.id == "npm"));
        assert!(providers.iter().any(|p| p.id == "jest"));
        assert!(providers.iter().any(|p| p.id == "vitest"));
        assert!(providers.iter().any(|p| p.id == "playwright"));
        assert!(providers.iter().any(|p| p.id == "mocha"));
        assert!(providers.iter().any(|p| p.id == "grunt"));
        assert!(providers.iter().any(|p| p.id == "gulp"));
        assert!(providers.iter().any(|p| p.id == "node"));
        assert!(providers.iter().any(|p| p.id == "compound"));
        assert!(providers.iter().any(|p| p.id == "react_native"));

        // Verify npm default template initializes correctly
        let npm_template = ConfigurationProvider::create_default("npm", Path::new("/workspace"), None);
        assert_eq!(npm_template.configuration_type.as_deref(), Some("npm"));
        assert!(npm_template.npm.is_some());

        // Verify compound template initializes correctly
        let compound_template = ConfigurationProvider::create_default("compound", Path::new("/workspace"), None);
        assert_eq!(compound_template.configuration_type.as_deref(), Some("compound"));
        assert!(compound_template.validate().is_ok());
    }
}
