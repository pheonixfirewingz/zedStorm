use super::*;
use editor::Editor;
use gpui::{DismissEvent, EventEmitter, FocusHandle, Focusable, Render};
use ui::{ContextMenu, PopoverMenu, Switch, SwitchLabelPosition, ToggleState, Tooltip, prelude::*};
use ui_input::InputField;
use uuid::Uuid;
use workspace::ModalView;

async fn detect_project_configuration(root: &Path) -> Option<RunConfiguration> {
    if !root.join("Cargo.toml").is_file() {
        return None;
    }
    let output = match util::command::new_command("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(root)
        .output()
        .await
    {
        Ok(output) => output,
        Err(error) => {
            log::warn!(
                "Could not detect Cargo run configuration for {}: {error}",
                root.display()
            );
            return None;
        }
    };
    if !output.status.success() {
        log::warn!(
            "Could not detect Cargo run configuration for {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    let metadata: Value = match serde_json::from_slice(&output.stdout) {
        Ok(metadata) => metadata,
        Err(error) => {
            log::warn!("Invalid Cargo metadata for {}: {error}", root.display());
            return None;
        }
    };
    cargo_configuration(&metadata, root)
}

fn cargo_configuration(metadata: &Value, root: &Path) -> Option<RunConfiguration> {
    let packages = metadata.get("packages")?.as_array()?;
    let members = metadata.get("workspace_default_members")?.as_array()?;
    let local_package = packages.iter().find(|package| {
        package
            .get("manifest_path")
            .and_then(Value::as_str)
            .is_some_and(|path| Path::new(path) == root.join("Cargo.toml"))
    });
    let mut binaries = Vec::new();
    for package in packages {
        if let Some(local_package) = local_package {
            if package != local_package {
                continue;
            }
        } else if !members.contains(package.get("id")?) {
            continue;
        }
        let default_run = package.get("default_run").and_then(Value::as_str);
        for target in package.get("targets")?.as_array()? {
            if !target
                .get("kind")?
                .as_array()?
                .iter()
                .any(|kind| kind.as_str() == Some("bin"))
            {
                continue;
            }
            let binary = target.get("name")?.as_str()?;
            if default_run.is_none_or(|default_run| default_run == binary) {
                binaries.push((package.get("name")?.as_str()?, binary));
            }
        }
    }
    // An ambiguous workspace must never launch an arbitrary binary.
    let [(package, binary)] = binaries.as_slice() else {
        return None;
    };
    let directory = Path::new(metadata.get("target_directory")?.as_str()?);
    let filename = format!("{binary}{}", std::env::consts::EXE_SUFFIX);
    let executable = ["debug", "release"]
        .into_iter()
        .filter_map(|profile| {
            let path = directory.join(profile).join(&filename);
            let file = std::fs::metadata(&path).ok()?;
            file.is_file()
                .then(|| (file.modified().ok(), profile, path))
        })
        .max_by_key(|(modified, _, _)| *modified);
    let mut arguments = vec![
        "run".into(),
        "--package".into(),
        (*package).into(),
        "--bin".into(),
        (*binary).into(),
    ];
    if executable
        .as_ref()
        .is_some_and(|(_, profile, _)| *profile == "release")
    {
        arguments.push("--release".into());
    }
    let debug = executable.map(|(_, _, path)| DebugScenario {
        adapter: "CodeLLDB".into(),
        label: (*binary).into(),
        build: None,
        config: json!({"request": "launch", "program": path, "cwd": root}),
        tcp_connection: None,
    });
    Some(RunConfiguration {
        name: (*binary).into(),
        run: Some(TaskTemplate {
            label: (*binary).into(),
            command: "cargo".into(),
            args: arguments,
            cwd: Some(root.to_string_lossy().into_owned()),
            ..TaskTemplate::default()
        }),
        debug,
    })
}

struct TargetFields {
    program: Entity<InputField>,
    cwd: Entity<InputField>,
    arguments: Vec<Entity<InputField>>,
    environment: Vec<(Entity<InputField>, Entity<InputField>)>,
}

impl TargetFields {
    fn new(
        program_label: &str,
        program: &str,
        cwd: &str,
        arguments: &[String],
        environment: &BTreeMap<String, String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        Self {
            program: input(program_label, program, window, cx),
            cwd: input("Working directory", cwd, window, cx),
            arguments: arguments
                .iter()
                .map(|argument| input("Argument", argument, window, cx))
                .collect(),
            environment: environment
                .iter()
                .map(|(name, value)| {
                    (
                        input("Name", name, window, cx),
                        input("Value", value, window, cx),
                    )
                })
                .collect(),
        }
    }

    fn arguments(&self, cx: &App) -> Vec<String> {
        self.arguments
            .iter()
            .map(|argument| argument.read(cx).text(cx))
            .collect()
    }

    fn environment(&self, cx: &App) -> Result<BTreeMap<String, String>> {
        let mut environment = BTreeMap::new();
        for (name, value) in &self.environment {
            let name = name.read(cx).text(cx);
            ensure!(
                !name.trim().is_empty() && !name.contains(['=', '\0']),
                "Environment variable names must be nonempty and cannot contain = or a null character"
            );
            ensure!(
                !environment.contains_key(&name),
                "Duplicate environment variable: {name}"
            );
            environment.insert(name, value.read(cx).text(cx));
        }
        Ok(environment)
    }
}

fn input(placeholder: &str, value: &str, window: &mut Window, cx: &mut App) -> Entity<InputField> {
    cx.new(|cx| {
        let field = InputField::new(window, cx, placeholder)
            .label(placeholder)
            .label_min_width(px(0.));
        field.set_text(value, window, cx);
        field
    })
}

struct ConfigurationDraft {
    entry: ConfigurationEntry,
    original: Option<ConfigurationEntry>,
    name: Entity<InputField>,
    run_enabled: bool,
    debug_enabled: bool,
    concurrent: bool,
    run: TargetFields,
    debug: TargetFields,
    adapter: Entity<InputField>,
    attach: bool,
    adapter_options: Entity<Editor>,
}

impl ConfigurationDraft {
    fn new(entry: ConfigurationEntry, saved: bool, window: &mut Window, cx: &mut App) -> Self {
        let configuration = &entry.configuration;
        let run = configuration.run.clone().unwrap_or_default();
        let debug = configuration.debug.as_ref();
        let debug_config = debug
            .map(|debug| debug.config.clone())
            .unwrap_or_else(|| json!({"request":"launch"}));
        let arguments = debug_config
            .get("args")
            .cloned()
            .map(serde_json::from_value::<Vec<String>>)
            .transpose();
        let environment = debug_config
            .get("env")
            .cloned()
            .map(serde_json::from_value::<BTreeMap<String, String>>)
            .transpose();
        let mut options = debug_config.clone();
        if let Some(object) = options.as_object_mut() {
            object.remove("request");
            for name in ["program", "cwd"] {
                if object.get(name).is_some_and(Value::is_string) {
                    object.remove(name);
                }
            }
            if arguments.is_ok() {
                object.remove("args");
            }
            if environment.is_ok() {
                object.remove("env");
            }
        }
        // Adapter-specific argument/environment shapes remain editable in the raw options.
        let arguments = match arguments {
            Ok(arguments) => arguments.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let environment = match environment {
            Ok(environment) => environment.unwrap_or_default(),
            Err(_) => BTreeMap::new(),
        };
        let options_text = serde_json::to_string_pretty(&options).unwrap_or_else(|error| {
            log::error!("Could not display adapter options: {error:#}");
            "{}".into()
        });
        let adapter_options = cx.new(|cx| {
            let mut editor = Editor::auto_height(3, 8, window, cx);
            editor.set_text(options_text, window, cx);
            editor
        });
        Self {
            original: saved.then(|| entry.clone()),
            name: input("Name", &configuration.name, window, cx),
            run_enabled: configuration.run.is_some(),
            debug_enabled: configuration.debug.is_some(),
            concurrent: run.allow_concurrent_runs,
            run: TargetFields::new(
                "Command",
                &run.command,
                run.cwd.as_deref().unwrap_or_default(),
                &run.args,
                &run.env.into_iter().collect(),
                window,
                cx,
            ),
            debug: TargetFields::new(
                "Program",
                debug_config
                    .get("program")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                debug_config
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                &arguments,
                &environment,
                window,
                cx,
            ),
            adapter: input(
                "Debug adapter",
                debug
                    .map(|debug| debug.adapter.as_ref())
                    .unwrap_or_default(),
                window,
                cx,
            ),
            attach: debug_config.get("request").and_then(Value::as_str) == Some("attach"),
            adapter_options,
            entry,
        }
    }

    fn apply_defaults(
        &mut self,
        defaults: Option<&RunConfiguration>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(debug) = defaults.and_then(|configuration| configuration.debug.as_ref()) else {
            return;
        };
        if self.entry.configuration.debug.is_some() {
            return;
        }
        self.adapter
            .update(cx, |field, cx| field.set_text(&debug.adapter, window, cx));
        for (field, key) in [(&self.debug.program, "program"), (&self.debug.cwd, "cwd")] {
            if let Some(value) = debug.config.get(key).and_then(Value::as_str) {
                field.update(cx, |field, cx| field.set_text(value, window, cx));
            }
        }
    }

    fn configuration(&self, cx: &App) -> Result<RunConfiguration> {
        let name = self.name.read(cx).text(cx);
        let run = if self.run_enabled {
            let mut run = self.entry.configuration.run.clone().unwrap_or_default();
            run.label = name.clone();
            run.command = self.run.program.read(cx).text(cx);
            run.args = self.run.arguments(cx);
            run.cwd = nonempty(self.run.cwd.read(cx).text(cx));
            run.env = self.run.environment(cx)?.into_iter().collect();
            run.allow_concurrent_runs = self.concurrent;
            Some(run)
        } else {
            None
        };
        let debug = if self.debug_enabled {
            let mut config: Value =
                settings_json::parse_json_with_comments(&self.adapter_options.read(cx).text(cx))
                    .context("Invalid adapter options")?;
            ensure!(config.is_object(), "Adapter options must be an object");
            config["request"] = json!(if self.attach { "attach" } else { "launch" });
            let program = self.debug.program.read(cx).text(cx);
            if !self.attach && !program.is_empty() {
                config["program"] = json!(program);
            }
            if let Some(cwd) = nonempty(self.debug.cwd.read(cx).text(cx)) {
                config["cwd"] = json!(cwd);
            }
            if !self.debug.arguments.is_empty() {
                config["args"] = json!(self.debug.arguments(cx));
            }
            if !self.debug.environment.is_empty() {
                config["env"] = json!(self.debug.environment(cx)?);
            }
            let mut debug =
                self.entry
                    .configuration
                    .debug
                    .clone()
                    .unwrap_or_else(|| DebugScenario {
                        adapter: "".into(),
                        label: "".into(),
                        build: None,
                        config: json!({}),
                        tcp_connection: None,
                    });
            debug.label = name.clone().into();
            debug.adapter = self.adapter.read(cx).text(cx).into();
            debug.config = config;
            Some(debug)
        } else {
            None
        };
        let configuration = RunConfiguration { name, run, debug };
        configuration.validate()?;
        Ok(configuration)
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

pub struct ConfigurationEditor {
    store: Entity<RunConfigurations>,
    drafts: Vec<ConfigurationDraft>,
    selected: Option<usize>,
    busy: bool,
    error: Option<String>,
    focus_handle: FocusHandle,
    show_options: bool,
    defaults: BTreeMap<PathBuf, RunConfiguration>,
    search: Entity<InputField>,
    _search_subscription: Subscription,
}

impl EventEmitter<DismissEvent> for ConfigurationEditor {}
impl ModalView for ConfigurationEditor {
    fn on_before_dismiss(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> workspace::DismissDecision {
        workspace::DismissDecision::Dismiss(!self.busy)
    }
}
impl Focusable for ConfigurationEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ConfigurationEditor {
    pub fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let Some(panel) = workspace.panel::<DebugPanel>(cx) else {
            return;
        };
        let store = panel.read(cx).run_configurations.clone();
        cx.spawn_in(window, async move |workspace, cx| {
            let refresh = store.update_in(cx, |store, window, cx| {
                store.refresh(window, cx);
                store.refresh_task.clone()
            })?;
            if let Some(refresh) = refresh {
                refresh.await;
            }
            wait_for_refresh(&store.downgrade(), cx).await?;
            let roots = store.read_with(cx, |store, _| store.roots.clone());
            let defaults = cx
                .background_spawn(async move {
                    let mut defaults = BTreeMap::new();
                    for (_, root) in roots {
                        if let Some(configuration) = detect_project_configuration(&root).await {
                            defaults.insert(root, configuration);
                        }
                    }
                    defaults
                })
                .await;
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.toggle_modal(window, cx, |window, cx| {
                    Self::new(store, defaults, window, cx)
                });
            })?;
            Ok(())
        })
        .detach_and_prompt_err(
            "Could not open run configurations",
            window,
            cx,
            |_, _, _| None,
        );
    }

    fn new(
        store: Entity<RunConfigurations>,
        defaults: BTreeMap<PathBuf, RunConfiguration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let drafts = store
            .read(cx)
            .entries
            .clone()
            .into_iter()
            .map(|entry| {
                let mut draft = ConfigurationDraft::new(entry, true, window, cx);
                draft.apply_defaults(defaults.get(&draft.entry.key.root), window, cx);
                draft
            })
            .collect::<Vec<_>>();
        let selected = drafts
            .iter()
            .position(|draft| Some(&draft.entry.key) == store.read(cx).selected.as_ref())
            .or_else(|| (!drafts.is_empty()).then_some(0));
        let error = store.read(cx).error.clone();
        let search = input("Search configurations", "", window, cx);
        let weak = cx.weak_entity();
        let search_editor = search.read(cx).editor().clone();
        let search_subscription = search_editor.subscribe(
            Box::new(move |event, _, cx| {
                if event == ui_input::ErasedEditorEvent::BufferEdited {
                    weak.update(cx, |_, cx| cx.notify()).log_err();
                }
            }),
            window,
            cx,
        );
        Self {
            store,
            drafts,
            selected,
            busy: false,
            error,
            focus_handle: cx.focus_handle(),
            show_options: false,
            defaults,
            search,
            _search_subscription: search_subscription,
        }
    }

    fn add(&mut self, duplicate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((worktree_id, root)) = self
            .store
            .read(cx)
            .roots
            .iter()
            .find(|(_, root)| Some(root) == self.store.read(cx).active_root.as_ref())
            .cloned()
        else {
            self.error = Some("Open a project folder before adding a configuration".into());
            cx.notify();
            return;
        };
        let base = if duplicate {
            self.selected
                .and_then(|index| self.drafts.get(index))
                .map(|draft| draft.configuration(cx))
                .transpose()
        } else {
            Ok(None)
        };
        let configuration = match base {
            Ok(Some(mut configuration)) => {
                configuration.name.push_str(" copy");
                configuration
            }
            Ok(None) => self
                .defaults
                .get(&root)
                .cloned()
                .unwrap_or_else(|| RunConfiguration {
                    name: "New configuration".into(),
                    run: Some(TaskTemplate::default()),
                    debug: None,
                }),
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let entry = ConfigurationEntry {
            key: ConfigurationKey {
                root,
                storage: Storage::Project,
                id: Uuid::new_v4().to_string(),
            },
            worktree_id,
            configuration,
        };
        let mut draft = ConfigurationDraft::new(entry, false, window, cx);
        draft.apply_defaults(self.defaults.get(&draft.entry.key.root), window, cx);
        draft.name.read(cx).focus_handle(cx).focus(window, cx);
        self.drafts.push(draft);
        self.selected = Some(self.drafts.len() - 1);
        self.error = None;
        cx.notify();
    }

    fn change_root(
        &mut self,
        worktree_id: WorktreeId,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.selected.and_then(|index| self.drafts.get_mut(index)) else {
            return;
        };
        if draft.original.is_some() {
            return;
        }
        let inferred = self
            .defaults
            .get(&draft.entry.key.root)
            .is_some_and(|configuration| configuration == &draft.entry.configuration);
        let unchanged = draft
            .configuration(cx)
            .is_ok_and(|configuration| configuration == draft.entry.configuration);
        draft.entry.key.root = root.clone();
        draft.entry.worktree_id = worktree_id;
        if inferred && unchanged {
            draft.entry.configuration =
                self.defaults
                    .get(&root)
                    .cloned()
                    .unwrap_or_else(|| RunConfiguration {
                        name: "New configuration".into(),
                        run: Some(TaskTemplate::default()),
                        debug: None,
                    });
            *draft = ConfigurationDraft::new(draft.entry.clone(), false, window, cx);
            draft.apply_defaults(self.defaults.get(&root), window, cx);
        }
        cx.notify();
    }

    fn save(
        &mut self,
        close: bool,
        launch: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(selected_draft) = self.drafts.get(selected) else {
            return;
        };
        let selected_key = selected_draft.entry.key.clone();
        let mut changes = Vec::new();
        for (index, draft) in self.drafts.iter().enumerate() {
            let configuration = match draft.configuration(cx) {
                Ok(configuration) => configuration,
                Err(error) => {
                    self.selected = Some(index);
                    self.error = Some(format!("{error:#}"));
                    cx.notify();
                    return;
                }
            };
            if index == selected
                && let Some(debug) = launch
            {
                if debug && configuration.debug.is_none() || !debug && configuration.run.is_none() {
                    self.error =
                        Some("This configuration does not support that launch mode".into());
                    cx.notify();
                    return;
                }
            }
            if draft.original.as_ref().is_none_or(|original| {
                original.key != draft.entry.key || original.configuration != configuration
            }) {
                changes.push((
                    index,
                    ConfigurationEntry {
                        configuration,
                        ..draft.entry.clone()
                    },
                    draft.original.clone(),
                ));
            }
        }
        let store = self.store.clone();
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                for (index, entry, original) in changes {
                    store
                        .update_in(cx, |store, window, cx| {
                            store.save(entry.clone(), original, window, cx)
                        })?
                        .await?;
                    this.update(cx, |this, _| {
                        if let Some(draft) = this.drafts.get_mut(index) {
                            draft.original = Some(entry.clone());
                            draft.entry = entry;
                        }
                    })?;
                }
                store.update(cx, |store, cx| store.select(selected_key, cx));
                anyhow::Ok(())
            }
            .await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(debug) = launch {
                            store.update(cx, |store, cx| store.launch_selected(debug, window, cx));
                        }
                        if close {
                            cx.emit(DismissEvent);
                        }
                    }
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
        cx.notify();
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(draft) = self.drafts.get(index) else {
            return;
        };
        let Some(original) = draft.original.clone() else {
            self.drafts.remove(index);
            self.selected =
                (!self.drafts.is_empty()).then_some(index.min(self.drafts.len().saturating_sub(1)));
            cx.notify();
            return;
        };
        let entry = original;
        let store = self.store.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let response = cx
                .prompt(
                    gpui::PromptLevel::Warning,
                    "Delete this configuration?",
                    Some("Running sessions will keep running."),
                    &["Delete", "Cancel"],
                )
                .await;
            let result = if response == Ok(0) {
                store
                    .update_in(cx, |store, window, cx| store.delete(entry, window, cx))?
                    .await
                    .map(|_| true)
            } else {
                Ok(false)
            };
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(true) => {
                        this.drafts.remove(index);
                        this.selected = (!this.drafts.is_empty())
                            .then_some(index.min(this.drafts.len().saturating_sub(1)));
                    }
                    Ok(false) => {}
                    Err(error) => this.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
        cx.notify();
    }

    fn render_target(&self, debug: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.selected.and_then(|index| self.drafts.get(index)) else {
            return div().into_any_element();
        };
        let fields = if debug { &draft.debug } else { &draft.run };
        let arguments = fields
            .arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                h_flex()
                    .gap_1()
                    .child(div().flex_1().child(argument.clone()))
                    .child(
                        IconButton::new(("remove-argument", index), IconName::Close)
                            .aria_label("Remove argument")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(draft) =
                                    this.selected.and_then(|index| this.drafts.get_mut(index))
                                {
                                    let fields = if debug {
                                        &mut draft.debug
                                    } else {
                                        &mut draft.run
                                    };
                                    if index < fields.arguments.len() {
                                        fields.arguments.remove(index);
                                    }
                                }
                                cx.notify();
                            })),
                    )
            })
            .collect::<Vec<_>>();
        let environment = fields
            .environment
            .iter()
            .enumerate()
            .map(|(index, (name, value))| {
                h_flex()
                    .gap_1()
                    .child(div().flex_1().child(name.clone()))
                    .child(div().flex_1().child(value.clone()))
                    .child(
                        IconButton::new(("remove-environment", index), IconName::Close)
                            .aria_label("Remove environment variable")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(draft) =
                                    this.selected.and_then(|index| this.drafts.get_mut(index))
                                {
                                    let fields = if debug {
                                        &mut draft.debug
                                    } else {
                                        &mut draft.run
                                    };
                                    if index < fields.environment.len() {
                                        fields.environment.remove(index);
                                    }
                                }
                                cx.notify();
                            })),
                    )
            })
            .collect::<Vec<_>>();
        v_flex()
            .id(if debug { "debug-target-fields" } else { "run-target-fields" })
            .gap_2()
            .w_full()
            .when(!debug || !draft.attach, |form| {
                if !debug || !fields.program.read(cx).text(cx).is_empty() || self.show_options {
                    form.child(fields.program.clone())
                } else {
                    form.child(
                        Button::new("choose-debug-executable", "Choose executable…")
                            .tooltip(Tooltip::text("No executable detected. Set a program or adapter-specific launch options."))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_options = true;
                                if let Some(draft) = this.selected.and_then(|index| this.drafts.get(index)) {
                                    draft.debug.program.read(cx).focus_handle(cx).focus(window, cx);
                                }
                                cx.notify();
                            })),
                    )
                }
            })
            .when(!self.show_options && (!fields.arguments.is_empty() || !fields.environment.is_empty()), |form| {
                form.child(Button::new(
                    if debug { "debug-extra-settings" } else { "run-extra-settings" },
                    match (fields.arguments.len(), fields.environment.len()) {
                        (0, environment) => format!("Environment variables ({environment})…"),
                        (arguments, 0) => format!("Arguments ({arguments})…"),
                        (arguments, environment) => format!("Arguments ({arguments}), environment ({environment})…"),
                    },
                ).on_click(cx.listener(|this, _, _, cx| {
                    this.show_options = true;
                    cx.notify();
                })))
            })
            .when(self.show_options, |form| {
                form.child(fields.cwd.clone())
            })
            .when((!debug || !draft.attach) && self.show_options, |form| {
                form.child(Label::new("Arguments"))
                    .children(arguments)
                    .child(
                        Button::new(
                            if debug {
                                "add-debug-argument"
                            } else {
                                "add-run-argument"
                            },
                            "Add argument",
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if let Some(draft) =
                                    this.selected.and_then(|index| this.drafts.get_mut(index))
                                {
                                    let fields = if debug {
                                        &mut draft.debug
                                    } else {
                                        &mut draft.run
                                    };
                                    fields.arguments.push(input("Argument", "", window, cx));
                                }
                                cx.notify();
                            },
                        )),
                    )
            })
            .when(self.show_options, |form| {
                form.child(Label::new("Environment variables"))
            .children(environment)
            .child(
                Button::new(
                    if debug {
                        "add-debug-variable"
                    } else {
                        "add-run-variable"
                    },
                    "Add variable",
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(draft) = this.selected.and_then(|index| this.drafts.get_mut(index))
                    {
                        let fields = if debug {
                            &mut draft.debug
                        } else {
                            &mut draft.run
                        };
                        fields.environment.push((
                            input("Name", "", window, cx),
                            input("Value", "", window, cx),
                        ));
                    }
                    cx.notify();
                })),
            )
            })
            .into_any_element()
    }
}

impl Render for ConfigurationEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = window.viewport_size().width < px(800.);
        let draft = self.selected.and_then(|index| self.drafts.get(index));
        let run_enabled = draft.is_some_and(|draft| draft.run_enabled);
        let debug_enabled = draft.is_some_and(|draft| draft.debug_enabled);
        let title = Label::new("Run / Debug Configurations").size(LabelSize::Large);
        let query = self.search.read(cx).text(cx).to_lowercase();
        let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, draft) in self.drafts.iter().enumerate() {
            if draft.name.read(cx).text(cx).to_lowercase().contains(&query) {
                groups
                    .entry(draft.entry.configuration.kind())
                    .or_default()
                    .push(index);
            }
        }
        let list = v_flex()
            .gap_2()
            .min_w_0()
            .child(self.search.clone())
            .children(groups.into_iter().map(|(kind, indices)| {
                v_flex()
                    .gap_1()
                    .child(Label::new(kind).color(Color::Muted))
                    .children(indices.into_iter().filter_map(|index| {
                        let draft = self.drafts.get(index)?;
                        Some(
                            Button::new(("configuration", index), draft.name.read(cx).text(cx))
                                .full_width()
                                .truncate(true)
                                .toggle_state(self.selected == Some(index))
                                .disabled(self.busy)
                                .tab_index(0_isize)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected = Some(index);
                                    this.error = None;
                                    cx.notify();
                                })),
                        )
                    }))
            }));
        let form = v_flex()
            .gap_3()
            .min_w_0()
            .flex_1()
            .when_some(draft, |form, draft| {
                form.child(draft.name.clone())
                    .child(
                        Button::new(
                            "configuration-advanced",
                            if self.show_options {
                                "Hide advanced options"
                            } else {
                                "Advanced…"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_options = !this.show_options;
                            cx.notify();
                        })),
                    )
                    .when(
                        self.show_options || self.store.read(cx).roots.len() > 1,
                        |form| {
                            form.child(
                                PopoverMenu::new("configuration-project-root")
                                    .trigger(
                                        Button::new(
                                            "configuration-project-root-button",
                                            format!("Project: {}", draft.entry.key.root.display()),
                                        )
                                        .truncate(true)
                                        .disabled(draft.original.is_some()),
                                    )
                                    .menu({
                                        let roots = self.store.read(cx).roots.clone();
                                        let editor = cx.weak_entity();
                                        move |window, cx| {
                                            Some(ContextMenu::build(
                                                window,
                                                cx,
                                                |mut menu, _, _| {
                                                    for (worktree_id, root) in &roots {
                                                        let root = root.clone();
                                                        let worktree_id = *worktree_id;
                                                        let editor = editor.clone();
                                                        menu = menu.entry(
                                                            root.display().to_string(),
                                                            None,
                                                            move |window, cx| {
                                                                editor
                                                                    .update(cx, |editor, cx| {
                                                                        editor.change_root(
                                                                            worktree_id,
                                                                            root.clone(),
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    })
                                                                    .log_err();
                                                            },
                                                        );
                                                    }
                                                    menu
                                                },
                                            ))
                                        }
                                    }),
                            )
                        },
                    )
                    .when(self.show_options, |form| {
                        form.child(
                            Switch::new(
                                "configuration-storage",
                                if draft.entry.key.storage == Storage::Project {
                                    ToggleState::Selected
                                } else {
                                    ToggleState::Unselected
                                },
                            )
                            .label("Share with project")
                            .label_position(SwitchLabelPosition::End)
                            .on_click(cx.listener(
                                |this, state: &ToggleState, _, cx| {
                                    if let Some(draft) =
                                        this.selected.and_then(|index| this.drafts.get_mut(index))
                                    {
                                        draft.entry.key.storage = if state.selected() {
                                            Storage::Project
                                        } else {
                                            Storage::Local
                                        };
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .child(
                        Switch::new(
                            "configuration-run-target",
                            if run_enabled {
                                ToggleState::Selected
                            } else {
                                ToggleState::Unselected
                            },
                        )
                        .label("Run target")
                        .label_position(SwitchLabelPosition::End)
                        .on_click(cx.listener(
                            |this, state: &ToggleState, _, cx| {
                                if let Some(draft) =
                                    this.selected.and_then(|index| this.drafts.get_mut(index))
                                {
                                    draft.run_enabled = state.selected();
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .when(run_enabled, |form| {
                        form.child(self.render_target(false, cx)).when(
                            self.show_options || draft.concurrent,
                            |form| {
                                form.child(
                                    Switch::new(
                                        "configuration-concurrent",
                                        if draft.concurrent {
                                            ToggleState::Selected
                                        } else {
                                            ToggleState::Unselected
                                        },
                                    )
                                    .label("Allow multiple instances")
                                    .label_position(SwitchLabelPosition::End)
                                    .on_click(cx.listener(
                                        |this, state: &ToggleState, _, cx| {
                                            if let Some(draft) = this
                                                .selected
                                                .and_then(|index| this.drafts.get_mut(index))
                                            {
                                                draft.concurrent = state.selected();
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                            },
                        )
                    })
                    .child(
                        Switch::new(
                            "configuration-debug-target",
                            if debug_enabled {
                                ToggleState::Selected
                            } else {
                                ToggleState::Unselected
                            },
                        )
                        .label("Debug target")
                        .label_position(SwitchLabelPosition::End)
                        .on_click(cx.listener(
                            |this, state: &ToggleState, _, cx| {
                                if let Some(draft) =
                                    this.selected.and_then(|index| this.drafts.get_mut(index))
                                {
                                    draft.debug_enabled = state.selected();
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .when(debug_enabled, |form| {
                        let adapters = DapRegistry::global(cx)
                            .enumerate_adapters::<Vec<dap::adapters::DebugAdapterName>>();
                        form.when(
                            !self.show_options && !draft.adapter.read(cx).text(cx).is_empty(),
                            |form| {
                                form.child(
                                    Label::new(format!(
                                        "Debugger: {}",
                                        draft.adapter.read(cx).text(cx)
                                    ))
                                    .color(Color::Muted),
                                )
                            },
                        )
                        .when(
                            self.show_options || draft.adapter.read(cx).text(cx).is_empty(),
                            |form| {
                                form.child(
                                    h_flex()
                                        .gap_1()
                                        .child(div().flex_1().child(draft.adapter.clone()))
                                        .child(
                                            PopoverMenu::new("configuration-adapter")
                                                .trigger(
                                                    IconButton::new(
                                                        "choose-configuration-adapter",
                                                        IconName::ChevronDown,
                                                    )
                                                    .aria_label("Choose debug adapter"),
                                                )
                                                .menu({
                                                    let adapter_field = draft.adapter.clone();
                                                    move |window, cx| {
                                                        Some(ContextMenu::build(
                                                            window,
                                                            cx,
                                                            |mut menu, _, _| {
                                                                for adapter in &adapters {
                                                                    let field =
                                                                        adapter_field.clone();
                                                                    let adapter = adapter.0.clone();
                                                                    menu = menu.entry(
                                                                        adapter.clone(),
                                                                        None,
                                                                        move |window, cx| {
                                                                            field.update(
                                                                                cx,
                                                                                |field, cx| {
                                                                                    field.set_text(
                                                                                        &adapter,
                                                                                        window, cx,
                                                                                    )
                                                                                },
                                                                            )
                                                                        },
                                                                    );
                                                                }
                                                                menu
                                                            },
                                                        ))
                                                    }
                                                }),
                                        ),
                                )
                            },
                        )
                        .when(self.show_options || draft.attach, |form| {
                            form.child(
                                Switch::new(
                                    "configuration-attach",
                                    if draft.attach {
                                        ToggleState::Selected
                                    } else {
                                        ToggleState::Unselected
                                    },
                                )
                                .label("Attach to a process")
                                .label_position(SwitchLabelPosition::End)
                                .on_click(cx.listener(
                                    |this, state: &ToggleState, _, cx| {
                                        if let Some(draft) = this
                                            .selected
                                            .and_then(|index| this.drafts.get_mut(index))
                                        {
                                            draft.attach = state.selected();
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                        })
                        .child(self.render_target(true, cx))
                        .when(self.show_options || draft.attach, |form| {
                            form.child(Label::new("Additional adapter options")).child(
                                div()
                                    .border_1()
                                    .border_color(cx.theme().colors().border)
                                    .rounded_md()
                                    .p_2()
                                    .child(draft.adapter_options.clone()),
                            )
                        })
                    })
            })
            .when(draft.is_none(), |form| {
                form.child(
                    Label::new("Add a configuration to choose what Run and Debug launch.")
                        .color(Color::Muted),
                )
            });
        let content = if compact {
            v_flex().gap_3().child(list).child(form).into_any_element()
        } else {
            h_flex()
                .gap_4()
                .items_start()
                .child(div().w(px(200.)).flex_shrink_0().child(list))
                .child(form)
                .into_any_element()
        };
        v_flex()
            .id("run-configurations-editor")
            .key_context("RunConfigurations")
            .track_focus(&self.focus_handle)
            .tab_group()
            .on_action(cx.listener(|this, _: &menu::Cancel, _, cx| {
                if !this.busy {
                    cx.emit(DismissEvent);
                }
            }))
            .on_action(cx.listener(|_, _: &menu::SelectNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &menu::SelectPrevious, window, cx| window.focus_prev(cx)))
            .w(px(860.).min(window.viewport_size().width - px(32.)))
            .max_h(window.viewport_size().height - px(80.))
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_lg()
            .shadow_lg()
            .child(
                v_flex().p_3().gap_2().child(title).child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("add-configuration", "Add")
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.add(false, window, cx)),
                                ),
                        )
                        .child(
                            Button::new("duplicate-configuration", "Duplicate")
                                .disabled(self.busy || draft.is_none())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.add(true, window, cx)),
                                ),
                        )
                        .child(
                            Button::new("remove-configuration", "Remove")
                                .disabled(self.busy || draft.is_none())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.remove(window, cx)),
                                ),
                        ),
                ),
            )
            .child(
                div()
                    .id("configuration-form-scroll")
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(content),
            )
            .when_some(self.error.clone(), |form, error| {
                form.child(
                    div()
                        .px_3()
                        .py_2()
                        .child(Label::new(error).color(Color::Error)),
                )
            })
            .child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .justify_end()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Button::new("cancel-configurations", "Cancel")
                            .disabled(self.busy)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new(
                            "apply-configuration",
                            if self.busy { "Saving…" } else { "Apply" },
                        )
                        .disabled(self.busy || draft.is_none())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.save(false, None, window, cx)),
                        ),
                    )
                    .child(
                        Button::new("save-configuration", "Save")
                            .disabled(self.busy || draft.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.save(true, None, window, cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new("run-draft", "Run")
                            .start_icon(Icon::new(IconName::PlayFilled))
                            .disabled(self.busy || !run_enabled)
                            .tooltip(Tooltip::text("Save and run this configuration"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save(true, Some(false), window, cx)
                            })),
                    )
                    .child(
                        Button::new("debug-draft", "Debug")
                            .start_icon(Icon::new(IconName::Debug))
                            .disabled(self.busy || !debug_enabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save(true, Some(true), window, cx)
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(root: &Path, directory: &Path) -> Value {
        json!({
            "target_directory": directory,
            "workspace_default_members": ["app-id"],
            "packages": [
                {
                    "id": "app-id",
                    "name": "application",
                    "manifest_path": root.join("member/Cargo.toml"),
                    "default_run": "application",
                    "targets": [
                        {"name": "application", "kind": ["bin"]},
                        {"name": "helper", "kind": ["bin"]}
                    ]
                },
                {
                    "id": "other-id",
                    "name": "other",
                    "manifest_path": root.join("other/Cargo.toml"),
                    "targets": [{"name": "other", "kind": ["bin"]}]
                }
            ]
        })
    }

    #[test]
    fn run_configurations_detect_cargo_binary_in_custom_output_directory() -> Result<()> {
        let root = std::env::temp_dir().join(format!("run-defaults-{}", Uuid::new_v4()));
        let directory = root.join("custom-output");
        let executable = directory
            .join("release")
            .join(format!("application{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(directory.join("release"))?;
        std::fs::write(&executable, "fixture")?;
        let configuration = cargo_configuration(&metadata(&root, &directory), &root)
            .context("Expected a configuration for the default binary")?;
        let debug = configuration
            .debug
            .context("Expected the existing executable")?;
        assert_eq!(debug.adapter.as_ref(), "CodeLLDB");
        assert_eq!(debug.config["program"], json!(executable));
        assert_eq!(debug.config["cwd"], json!(root));
        let run = configuration.run.context("Expected a run target")?;
        assert_eq!(
            run.args,
            [
                "run",
                "--package",
                "application",
                "--bin",
                "application",
                "--release"
            ]
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn run_configurations_do_not_invent_missing_or_ambiguous_executables() -> Result<()> {
        let root = std::env::temp_dir().join(format!("run-defaults-{}", Uuid::new_v4()));
        let mut metadata = metadata(&root, &root.join("target"));
        let configuration =
            cargo_configuration(&metadata, &root).context("Expected a run target")?;
        assert!(configuration.debug.is_none());
        metadata["packages"][0]["default_run"] = Value::Null;
        assert!(cargo_configuration(&metadata, &root).is_none());
        metadata["packages"][0]["targets"] = json!([{"name": "application", "kind": ["lib"]}]);
        assert!(cargo_configuration(&metadata, &root).is_none());
        Ok(())
    }

    #[test]
    fn run_configurations_prefer_open_package_over_workspace_defaults() -> Result<()> {
        let root = std::env::temp_dir().join(format!("run-defaults-{}", Uuid::new_v4()));
        let metadata = metadata(&root, &root.join("target"));
        let configuration = cargo_configuration(&metadata, &root.join("other"))
            .context("Expected the opened member's run target")?;
        assert_eq!(configuration.name, "other");
        Ok(())
    }
}
