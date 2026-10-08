mod configuration_editor;

use anyhow::{Context as _, Result, ensure};
use collections::{BTreeMap, HashMap};
use dap::DapRegistry;
use db::kvp::KeyValueStore;
use fs::Fs;
use futures::{FutureExt as _, future::Shared};
use gpui::{App, AppContext, Context, Entity, Subscription, Task, WeakEntity, Window, actions};
use project::{Project, TaskContexts, TaskSourceKind, WorktreeId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use task::{DebugScenario, TaskTemplate};
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
        /// Debugs the selected configuration.
        DebugSelected,
    ]
);

const CONFIGURATION_PATH: &str = ".zed/run.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunConfiguration {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<TaskTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<DebugScenario>,
}

impl RunConfiguration {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.trim().is_empty(), "Enter a configuration name");
        ensure!(
            self.run.is_some() || self.debug.is_some(),
            "Add a Run or Debug target"
        );
        if let Some(run) = &self.run {
            ensure!(
                !run.command.trim().is_empty(),
                "Enter a command for the Run target"
            );
        }
        if let Some(debug) = &self.debug {
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
        match (&self.run, &self.debug) {
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
        for key in ["name", "run", "debug"] {
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
    pub loading: bool,
    pub error: Option<String>,
    roots: Vec<(WorktreeId, PathBuf)>,
    active_root: Option<PathBuf>,
    contexts: Arc<TaskContexts>,
    project_files: HashMap<PathBuf, String>,
    local_states: HashMap<PathBuf, LocalState>,
    refresh_task: Option<Shared<Task<()>>>,
    refresh_generation: usize,
    selection_save_task: Option<Task<()>>,
    subscriptions: Vec<Subscription>,
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
                loading: false,
                error: None,
                roots: Vec::new(),
                active_root: None,
                contexts: Arc::new(TaskContexts::default()),
                project_files: HashMap::default(),
                local_states: HashMap::default(),
                refresh_task: None,
                refresh_generation: 0,
                selection_save_task: None,
                subscriptions: Vec::new(),
            };
            if let Some(workspace_handle) = workspace_handle.upgrade() {
                this.subscriptions.push(cx.subscribe_in(
                    &workspace_handle,
                    window,
                    |this, _, event: &workspace::Event, window, cx| {
                        if matches!(event, workspace::Event::ActiveItemChanged) {
                            this.refresh(window, cx);
                        }
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
                            .any(|(path, _, _)| path.as_unix_str() == CONFIGURATION_PATH),
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
            .find(|entry| Some(&entry.key) == self.selected.as_ref())
    }

    pub fn selected_label(&self) -> String {
        self.selected_entry()
            .map(|entry| entry.configuration.name.clone())
            .unwrap_or_else(|| "Select configuration".into())
    }

    pub fn can_launch(&self, debug: bool, cx: &App) -> bool {
        !self.loading
            && self.selected_entry().is_some_and(|entry| {
                entry.configuration.validate().is_ok()
                    && if debug {
                        entry.configuration.debug.as_ref().is_some_and(|debug| {
                            cx.global::<DapRegistry>().adapter(&debug.adapter).is_some()
                        })
                    } else {
                        entry.configuration.run.is_some()
                    }
            })
    }

    pub fn select(&mut self, key: ConfigurationKey, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
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

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok((contexts, roots, local_project)) = self.workspace.update(cx, |workspace, cx| {
            let contexts = tasks_ui::task_contexts(workspace, window, cx);
            let roots = workspace
                .visible_worktrees(cx)
                .filter_map(|worktree| {
                    let worktree = worktree.read(cx);
                    worktree.root_entry().filter(|entry| entry.is_dir())?;
                    Some((worktree.id(), worktree.abs_path().to_path_buf()))
                })
                .collect::<Vec<_>>();
            (contexts, roots, workspace.project().read(cx).is_local())
        }) else {
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
                let loaded = cx
                    .background_spawn(async move {
                        let mut files = Vec::new();
                        for root in root_paths {
                            let path = root.join(CONFIGURATION_PATH);
                            let result = if !local_project || fs.metadata(&path).await?.is_none() {
                                Ok(String::new())
                            } else {
                                fs.load(&path).await
                            };
                            files.push((root, result));
                        }
                        anyhow::Ok(files)
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
                        Ok(files) => {
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
                    this.selected = this
                        .active_root
                        .as_ref()
                        .and_then(|root| this.local_states.get(root))
                        .and_then(|state| state.selected.clone());
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
            let (entry, contexts, workspace) = this.read_with(cx, |this, _| {
                (
                    this.entries.iter().find(|entry| entry.key == key).cloned(),
                    this.contexts.clone(),
                    this.workspace.clone(),
                )
            })?;
            let entry =
                entry.context("The selected configuration was removed or could not be loaded")?;
            workspace.update_in(cx, |workspace, window, cx| {
                launch_configuration(workspace, &entry, &contexts, debug, window, cx)
            })??;
            Ok(())
        })
        .detach_and_prompt_err("Could not launch configuration", window, cx, |_, _, _| None);
    }

    fn save(
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
            let expected = expected.context("Reload configurations before saving; run.json could not be read")?;
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
                    // Store the destination first so a failed project-file write never loses a configuration.
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

    fn delete(
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
                    let expected = expected.context("Missing project file snapshot")?;
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

fn launch_configuration(
    workspace: &mut Workspace,
    entry: &ConfigurationEntry,
    contexts: &TaskContexts,
    debug: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    entry.configuration.validate()?;
    let context = if contexts.worktree() == Some(entry.worktree_id) {
        contexts.active_context()
    } else {
        contexts.task_context_for_worktree_id(entry.worktree_id)
    }
    .context("The configuration's project root is no longer open")?;
    if debug {
        let mut scenario = entry
            .configuration
            .debug
            .clone()
            .context("This configuration does not have a Debug target")?;
        ensure!(
            DapRegistry::global(cx).adapter(&scenario.adapter).is_some(),
            "The {} debug adapter is unavailable",
            scenario.adapter
        );
        scenario.label = entry.configuration.name.clone().into();
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
            panel.start_session(
                scenario,
                context.clone().into(),
                buffer,
                Some(entry.worktree_id),
                window,
                cx,
            )
        });
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
        let shell = task::ShellBuilder::new(&resolved.resolved.shell, cfg!(windows)).kind();
        // Task terminals concatenate arguments as shell text; form rows represent literal arguments.
        for argument in &mut resolved.resolved.args {
            *argument = shell
                .try_quote(argument)
                .context("Could not quote a configuration argument for the selected shell")?
                .into_owned();
        }
        workspace.schedule_resolved_task(source, resolved, false, window, cx);
    }
    Ok(())
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace
            .register_action(|workspace, _: &EditConfigurations, window, cx| {
                ConfigurationEditor::show(workspace, window, cx);
            })
            .register_action(|workspace, _: &RunSelected, window, cx| {
                launch_selected_action(workspace, false, window, cx);
            })
            .register_action(|workspace, _: &DebugSelected, window, cx| {
                launch_selected_action(workspace, true, window, cx);
            });
    })
    .detach();
}

fn launch_selected_action(
    workspace: &Workspace,
    debug: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(panel) = workspace.panel::<DebugPanel>(cx) {
        let configurations = panel.read(cx).run_configurations.clone();
        // Resolve the editor context after the Workspace action handler releases its lease.
        cx.spawn_in(window, async move |_, cx| {
            configurations
                .update_in(cx, |configurations, window, cx| {
                    configurations.launch_selected(debug, window, cx)
                })
                .log_err();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration(name: &str) -> RunConfiguration {
        RunConfiguration {
            name: name.into(),
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
                name: "Empty".into(),
                run: None,
                debug: None
            }
            .validate()
            .is_err()
        );
        assert!(configuration(" ").validate().is_err());
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
            .load(Path::new("/run-second/.zed/run.json"))
            .await
            .expect("shared file");
        assert_eq!(
            parse_configuration_file(&contents)
                .expect("valid configurations")
                .get("stable-id")
                .expect("stable identity")
                .name,
            "Renamed server"
        );
        let original = entry.clone();
        entry.key.storage = Storage::Local;
        window
            .update(cx, |_, window, cx| {
                store.update(cx, |store, cx| {
                    store.save(entry.clone(), Some(original), window, cx)
                })
            })
            .expect("window")
            .await
            .expect("move to local storage");
        let contents = fs
            .load(Path::new("/run-second/.zed/run.json"))
            .await
            .expect("shared file");
        assert!(
            parse_configuration_file(&contents)
                .expect("valid configurations")
                .is_empty()
        );
        store.read_with(cx, |store, _| {
            assert_eq!(store.selected.as_ref(), Some(&entry.key));
            assert_eq!(
                store
                    .selected_entry()
                    .expect("local entry")
                    .configuration
                    .name,
                "Renamed server"
            );
        });
    }

    #[gpui::test]
    async fn test_run_configuration_rejects_external_edits(cx: &mut gpui::TestAppContext) {
        let (fs, window, store) = test_workspace(cx).await;
        let (worktree_id, root) = store
            .read_with(cx, |store, _| store.roots.first().cloned())
            .expect("project root");
        let entry = ConfigurationEntry {
            key: ConfigurationKey {
                root: root.clone(),
                storage: Storage::Project,
                id: "server".into(),
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
            .expect("initial save");
        let path = root.join(CONFIGURATION_PATH);
        let original_text = fs.load(&path).await.expect("shared file");
        let external_text = configuration_file_edit(
            &original_text,
            "server",
            Some(&configuration("Changed elsewhere")),
        )
        .expect("external edit");
        fs.atomic_write(path.clone(), external_text.clone())
            .await
            .expect("external write");
        let mut edited = entry.clone();
        edited.configuration.name = "Draft name".into();
        let result = window
            .update(cx, |_, window, cx| {
                store.update(cx, |store, cx| store.save(edited, Some(entry), window, cx))
            })
            .expect("window")
            .await;
        assert!(result.is_err());
        assert_eq!(fs.load(&path).await.expect("preserved file"), external_text);
    }
}
