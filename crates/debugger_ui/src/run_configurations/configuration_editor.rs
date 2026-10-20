use super::*;
use editor::Editor;
use gpui::{DismissEvent, EventEmitter, FocusHandle, Focusable, Render};
use task::BuildTaskDefinition;
use ui::{
    Button, ContextMenu, FluentBuilder,
    IconButton, IconName, IconSize, Label, LabelSize, PopoverMenu, Switch, SwitchLabelPosition,
    ToggleState, Tooltip, prelude::*,
};
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
    let arguments = vec![
        "--package".into(),
        (*package).into(),
        "--bin".into(),
        (*binary).into(),
    ];
    let task = |action: &str| TaskTemplate {
        label: (*binary).into(),
        command: "cargo".into(),
        args: std::iter::once(action.to_owned())
            .chain(arguments.iter().cloned())
            .collect(),
        cwd: Some(root.to_string_lossy().into_owned()),
        ..TaskTemplate::default()
    };
    Some(RunConfiguration {
        debug_enabled: false,
        name: (*binary).into(),
        configuration_type: Some("cargo".into()),
        folder: None,
        allow_multiple_instances: false,
        before_launch: Vec::new(),
        npm: None,
        run: Some(task("run")),
        debug: Some(DebugScenario {
            adapter: "CodeLLDB".into(),
            label: (*binary).into(),
            build: Some(BuildTaskDefinition::Template {
                task_template: task("build"),
                locator_name: Some("rust-cargo-locator".into()),
            }),
            config: json!({"request": "launch", "cwd": root, "sourceLanguages": ["rust"]}),
            tcp_connection: None,
        }),
    })
}

fn input(
    placeholder: &'static str,
    value: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputField> {
    cx.new(|cx| {
        let field = InputField::new(window, cx, placeholder).label_min_width(px(0.));
        field.set_text(value, window, cx);
        field
    })
}

pub(crate) struct TargetFields {
    program: Entity<InputField>,
    cwd: Entity<InputField>,
    arguments: Vec<Entity<InputField>>,
    environment: Vec<(Entity<InputField>, Entity<InputField>)>,
}

impl TargetFields {
    fn new(
        program_label: &'static str,
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
                        input("Variable", name, window, cx),
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
            .filter(|argument| !argument.is_empty())
            .collect()
    }

    fn environment(&self, cx: &App) -> Result<BTreeMap<String, String>> {
        let mut environment = BTreeMap::new();
        for (name, value) in &self.environment {
            let name = name.read(cx).text(cx);
            let value = value.read(cx).text(cx);
            if name.is_empty() {
                continue;
            }
            ensure!(!name.contains('='), "Variable names cannot contain '='");
            environment.insert(name, value);
        }
        Ok(environment)
    }
}

pub struct ConfigurationDraft {
    pub entry: ConfigurationEntry,
    pub original: Option<ConfigurationEntry>,
    pub name: Entity<InputField>,
    pub allow_multiple_instances: bool,
    pub folder: Option<String>,
    pub before_launch: Vec<BeforeLaunchTask>,
    pub before_launch_expanded: bool,

    // Npm tab and fields
    pub npm_tab: usize, // 0 = Configuration, 1 = Browser / Live Edit
    pub npm_package_json: Entity<InputField>,
    pub npm_command: Entity<InputField>,
    pub npm_script: Entity<InputField>,
    pub npm_arguments: Entity<InputField>,
    pub npm_node_runtime: Entity<InputField>,
    pub npm_node_options: Entity<InputField>,
    pub npm_package_manager: Entity<InputField>,
    pub npm_environment: Vec<(Entity<InputField>, Entity<InputField>)>,
    pub npm_browser_live_edit: BrowserLiveEdit,

    // Generic / Debug targets
    pub run_enabled: bool,
    pub debug_enabled: bool,
    pub concurrent: bool,
    pub run: TargetFields,
    pub debug: TargetFields,
    pub adapter: Entity<InputField>,
    pub attach: bool,
    pub adapter_options: Entity<Editor>,
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

        // Initialize NPM fields if present
        let npm = configuration.npm.clone().unwrap_or_default();
        let npm_package_json = input("package.json", &npm.package_json, window, cx);
        let npm_command = input(
            "npm command",
            if npm.command.is_empty() {
                "run"
            } else {
                &npm.command
            },
            window,
            cx,
        );
        let npm_script = input("Script name from package.json", &npm.script, window, cx);
        let npm_arguments = input("Arguments", &npm.arguments, window, cx);
        let npm_node_options = input("Node options", &npm.node_options, window, cx);
        let npm_environment = npm
            .environment
            .iter()
            .map(|(k, v)| (input("Name", k, window, cx), input("Value", v, window, cx)))
            .collect();
        let npm_browser_live_edit = npm.browser_live_edit.unwrap_or_default();

        Self {
            original: saved.then(|| entry.clone()),
            name: input("Name", &configuration.name, window, cx),
            allow_multiple_instances: configuration.allow_multiple_instances,
            folder: configuration.folder.clone(),
            before_launch: configuration.before_launch.clone(),
            before_launch_expanded: false,

            npm_tab: 0,
            npm_package_json,
            npm_command,
            npm_script,
            npm_arguments,
            npm_node_runtime: input(
                "Node executable path (empty = Project)",
                npm.node_runtime.as_deref().unwrap_or_default(),
                window,
                cx,
            ),
            npm_node_options,
            npm_package_manager: input(
                "Package manager path (empty = npm)",
                npm.package_manager.as_deref().unwrap_or_default(),
                window,
                cx,
            ),
            npm_environment,
            npm_browser_live_edit,

            run_enabled: configuration.run.is_some(),
            debug_enabled: configuration.debug_enabled,
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
                "Debugger",
                debug
                    .map(|debug| debug.adapter.as_ref())
                    .unwrap_or("CodeLLDB"),
                window,
                cx,
            ),
            attach: debug_config.get("request").and_then(Value::as_str) == Some("attach"),
            adapter_options,
            entry,
        }
    }

    #[allow(dead_code)]
    fn builds_cargo_target(&self) -> bool {
        self.debug_enabled
            && !self.attach
            && self
                .entry
                .configuration
                .debug
                .as_ref()
                .is_some_and(|debug| {
                    matches!(
                        &debug.build,
                        Some(BuildTaskDefinition::Template { locator_name: Some(name), .. })
                            if name.as_ref() == "rust-cargo-locator"
                    )
                })
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
        self.entry.configuration.debug = Some(debug.clone());
    }

    pub fn is_npm(&self) -> bool {
        self.entry.configuration.npm.is_some()
            || self.entry.configuration.configuration_type.as_deref() == Some("npm")
    }

    pub fn configuration(&self, cx: &App) -> Result<RunConfiguration> {
        let name = self.name.read(cx).text(cx);

        if self.is_npm() {
            let mut env = BTreeMap::new();
            for (k, v) in &self.npm_environment {
                let k_str = k.read(cx).text(cx);
                let v_str = v.read(cx).text(cx);
                if !k_str.is_empty() {
                    env.insert(k_str, v_str);
                }
            }

            let npm_fields = NpmConfigurationFields {
                package_json: self.npm_package_json.read(cx).text(cx),
                command: self.npm_command.read(cx).text(cx),
                script: self.npm_script.read(cx).text(cx),
                arguments: self.npm_arguments.read(cx).text(cx),
                node_runtime: nonempty(self.npm_node_runtime.read(cx).text(cx)),
                node_options: self.npm_node_options.read(cx).text(cx),
                package_manager: nonempty(self.npm_package_manager.read(cx).text(cx)),
                environment: env,
                browser_live_edit: Some(self.npm_browser_live_edit.clone()),
            };

            let config = RunConfiguration {
                debug_enabled: self.debug_enabled,
                name,
                configuration_type: Some("npm".into()),
                folder: self.folder.clone(),
                allow_multiple_instances: self.allow_multiple_instances,
                before_launch: self.before_launch.clone(),
                npm: Some(npm_fields),
                run: None,
                debug: None,
            };
            config.validate()?;
            return Ok(config);
        }

        let run_cmd = self.run.program.read(cx).text(cx);
        let run = if self.run_enabled && !run_cmd.trim().is_empty() {
            let mut run = self.entry.configuration.run.clone().unwrap_or_default();
            run.label = name.clone();
            run.command = run_cmd;
            run.args = self.run.arguments(cx);
            run.cwd = nonempty(self.run.cwd.read(cx).text(cx));
            run.env = self.run.environment(cx)?.into_iter().collect();
            run.allow_concurrent_runs = self.concurrent;
            Some(run)
        } else {
            None
        };

        let debug = if self.debug_enabled {
            let adapter_options_text = self.adapter_options.read(cx).text(cx);
            let mut config: Value = if adapter_options_text.trim().is_empty() {
                json!({})
            } else {
                settings_json::parse_json_with_comments(&adapter_options_text)
                    .context("Invalid adapter options")?
            };
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
            let adapter_text = self.adapter.read(cx).text(cx);
            debug.adapter = if !adapter_text.trim().is_empty() {
                adapter_text.into()
            } else if !debug.adapter.is_empty() {
                debug.adapter
            } else {
                "CodeLLDB".into()
            };
            if self.attach
                || config.get("program").is_some()
                || config.get("targetCreateCommands").is_some()
            {
                if let Some(BuildTaskDefinition::Template { locator_name, .. }) = &mut debug.build {
                    *locator_name = None;
                }
            }
            debug.config = config;
            Some(debug)
        } else {
            self.entry.configuration.debug.clone()
        };

        let config = RunConfiguration {
            debug_enabled: self.debug_enabled,
            name,
            configuration_type: self.entry.configuration.configuration_type.clone(),
            folder: self.folder.clone(),
            allow_multiple_instances: self.allow_multiple_instances,
            before_launch: self.before_launch.clone(),
            npm: None,
            run,
            debug,
        };
        config.validate()?;
        Ok(config)
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
    _show_options: bool,
    defaults: BTreeMap<PathBuf, RunConfiguration>,
    search: Entity<InputField>,
    _search_subscription: Subscription,
    sort_alphabetical: bool,
    editing_templates: bool,
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
        let mut editor = Self {
            store,
            drafts,
            selected,
            busy: false,
            error,
            focus_handle: cx.focus_handle(),
            _show_options: false,
            defaults,
            search,
            _search_subscription: search_subscription,
            sort_alphabetical: false,
            editing_templates: false,
        };
        if editor.drafts.is_empty()
            && editor
                .store
                .read(cx)
                .active_root
                .as_ref()
                .is_some_and(|root| editor.defaults.contains_key(root))
        {
            editor.add("cargo", window, cx);
        }
        editor
    }

    pub fn has_changes(&self, cx: &App) -> bool {
        for draft in &self.drafts {
            if draft.original.is_none() {
                return true;
            }
            if let Some(original) = &draft.original {
                if draft.entry.key != original.key {
                    return true;
                }
                match draft.configuration(cx) {
                    Ok(current) => {
                        if current != original.configuration {
                            return true;
                        }
                    }
                    Err(_) => return true,
                }
            }
        }
        false
    }

    fn add(&mut self, type_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((worktree_id, root)) = self
            .store
            .read(cx)
            .roots
            .iter()
            .find(|(_, root)| Some(root) == self.store.read(cx).active_root.as_ref())
            .cloned()
            .or_else(|| self.store.read(cx).roots.first().cloned())
        else {
            self.error = Some("Open a project folder before adding a configuration".into());
            cx.notify();
            return;
        };

        // Check if there is an existing package.json in root
        let pkg_path = root.join("package.json");
        let configuration = if type_id == "cargo" {
            self.defaults.get(&root).cloned().unwrap_or_else(|| {
                registry::ConfigurationProvider::create_default(
                    type_id,
                    &root,
                    pkg_path.is_file().then_some(&pkg_path),
                )
            })
        } else {
            registry::ConfigurationProvider::create_default(
                type_id,
                &root,
                pkg_path.is_file().then_some(&pkg_path),
            )
        };

        let entry = ConfigurationEntry {
            key: ConfigurationKey {
                root: root.clone(),
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

    fn duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(draft) = self.drafts.get(selected) else {
            return;
        };
        let mut config = match draft.configuration(cx) {
            Ok(c) => c,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        config.name = format!("{} (1)", config.name);

        let entry = ConfigurationEntry {
            key: ConfigurationKey {
                root: draft.entry.key.root.clone(),
                storage: draft.entry.key.storage.clone(),
                id: Uuid::new_v4().to_string(),
            },
            worktree_id: draft.entry.worktree_id,
            configuration: config,
        };

        let new_draft = ConfigurationDraft::new(entry, false, window, cx);
        new_draft.name.read(cx).focus_handle(cx).focus(window, cx);
        self.drafts.push(new_draft);
        self.selected = Some(self.drafts.len() - 1);
        self.error = None;
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
                if debug && !configuration.debug_enabled {
                    self.error = Some("Enable debugging before starting a debug session".into());
                    cx.notify();
                    return;
                }
                if !draft.is_npm() {
                    if debug && configuration.debug.is_none() || !debug && configuration.run.is_none() {
                        self.error =
                            Some("This configuration does not support that launch mode".into());
                        cx.notify();
                        return;
                    }
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

    fn render_npm_form(&self, draft: &ConfigurationDraft, cx: &mut Context<Self>) -> AnyElement {
        let tab = draft.npm_tab;
        let store = self.store.read(cx);

        let root = &draft.entry.key.root;

        let discovered_packages: Vec<String> = store
            .discovered_package_scripts
            .keys()
            .filter_map(|p| {
                p.strip_prefix(root)
                    .ok()
                    .map(|rel| rel.to_string_lossy().into_owned())
            })
            .collect();

        let node_version_str = store
            .node_version
            .as_deref()
            .map(|v| format!("Project node, version {v}"))
            .unwrap_or_else(|| "Project node".into());

        let npm_version_str = store
            .npm_version
            .as_deref()
            .map(|v| format!("Project npm, version {v}"))
            .unwrap_or_else(|| "Project npm".into());

        v_flex()
            .gap_3()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .child(
                h_flex()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Button::new("npm-config-tab", "Configuration")
                            .toggle_state(tab == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                    d.npm_tab = 0;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("npm-browser-tab", "Browser / Live Edit")
                            .toggle_state(tab == 1)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                    d.npm_tab = 1;
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(if tab == 0 {
                v_flex()
                    .gap_3()
                    .w_full()
                    // 1. package.json field
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("package.json").size(LabelSize::Small).color(Color::Muted))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(div().flex_1().min_w_0().child(draft.npm_package_json.clone()))
                                    .child(
                                        PopoverMenu::new("choose-pkg-json")
                                            .trigger(IconButton::new("choose-pkg-btn", IconName::ChevronDown))
                                            .menu({
                                                let editor = cx.weak_entity();
                                                let pkgs = discovered_packages.clone();
                                                move |window, cx| {
                                                    let editor = editor.clone();
                                                    let pkgs = pkgs.clone();
                                                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                                        for pkg in pkgs {
                                                            let editor = editor.clone();
                                                            let pkg_clone = pkg.clone();
                                                            menu = menu.entry(pkg, None, move |window, cx| {
                                                                editor.update(cx, |this, cx| {
                                                                    if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                                                        d.npm_package_json.update(cx, |f, cx| f.set_text(&pkg_clone, window, cx));
                                                                        cx.notify();
                                                                    }
                                                                }).log_err();
                                                            });
                                                        }
                                                        menu
                                                    }))
                                                }
                                            })
                                    ),
                            ),
                    )
                    // 2. Command dropdown
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Command").size(LabelSize::Small).color(Color::Muted))
                            .child(draft.npm_command.clone())
                            .child(
                                PopoverMenu::new("npm-command-menu")
                                    .trigger(Button::new("npm-command-val", "Choose npm command").end_icon(Icon::new(IconName::ChevronDown)))
                                    .menu({
                                        let editor = cx.weak_entity();
                                        move |window, cx| {
                                            let editor = editor.clone();
                                            Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                                for cmd in ["run", "run-script", "start", "test", "stop", "restart", "install", "ci"] {
                                                    let editor = editor.clone();
                                                    menu = menu.entry(cmd, None, move |window, cx| {
                                                        editor.update(cx, |this, cx| {
                                                            if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                                                d.npm_command.update(cx, |field, cx| field.set_text(cmd, window, cx));
                                                                cx.notify();
                                                            }
                                                        }).log_err();
                                                    });
                                                }
                                                menu
                                            }))
                                        }
                                    })
                            ),
                    )
                    // 3. Scripts dropdown
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Scripts").size(LabelSize::Small).color(Color::Muted))
                            .child(draft.npm_script.clone())
                            .child(
                                PopoverMenu::new("npm-scripts-menu")
                                    .trigger(
                                        Button::new(
                                            "npm-script-val",
                                            "Choose from package.json",
                                        )
                                        .end_icon(Icon::new(IconName::ChevronDown))
                                    )
                                    .menu({
                                        let editor = cx.weak_entity();
                                        move |window, cx| {
                                            let editor = editor.clone();
                                            let list = editor.read_with(cx, |this, cx| {
                                                let draft = this.selected.and_then(|index| this.drafts.get(index))?;
                                                let package_path = draft.entry.key.root.join(draft.npm_package_json.read(cx).text(cx));
                                                this.store.read(cx).discovered_package_scripts.get(&package_path)
                                                    .map(|scripts| scripts.keys().cloned().collect::<Vec<_>>())
                                            }).log_err().flatten().unwrap_or_default();
                                            Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                                for script in list {
                                                    let editor = editor.clone();
                                                    let s_clone = script.clone();
                                                    menu = menu.entry(script, None, move |window, cx| {
                                                        editor.update(cx, |this, cx| {
                                                            if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                                                d.npm_script.update(cx, |field, cx| field.set_text(&s_clone, window, cx));
                                                                cx.notify();
                                                            }
                                                        }).log_err();
                                                    });
                                                }
                                                menu
                                            }))
                                        }
                                    })
                            ),
                    )
                    // 4. Arguments
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Arguments").size(LabelSize::Small).color(Color::Muted))
                            .child(draft.npm_arguments.clone()),
                    )
                    // 5. Node runtime
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Node runtime").size(LabelSize::Small).color(Color::Muted))
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(Label::new(node_version_str).size(LabelSize::Small))
                                    .child(draft.npm_node_runtime.clone()),
                            ),
                    )
                    // 6. Node options
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Node options").size(LabelSize::Small).color(Color::Muted))
                            .child(draft.npm_node_options.clone()),
                    )
                    // 7. Package manager
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Package manager").size(LabelSize::Small).color(Color::Muted))
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(Label::new(npm_version_str).size(LabelSize::Small))
                                    .child(draft.npm_package_manager.clone()),
                            ),
                    )
                    // 8. Environment
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Environment").size(LabelSize::Small).color(Color::Muted))
                            .children(draft.npm_environment.iter().enumerate().map(|(idx, (k, v))| {
                                h_flex()
                                    .gap_1()
                                    .child(div().flex_1().min_w_0().child(k.clone()))
                                    .child(div().flex_1().min_w_0().child(v.clone()))
                                    .child(
                                        IconButton::new(("del-env", idx), IconName::Close)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                                    if idx < d.npm_environment.len() {
                                                        d.npm_environment.remove(idx);
                                                        cx.notify();
                                                    }
                                                }
                                            }))
                                    )
                            }))
                            .child(
                                Button::new("add-npm-env", "Add environment variable")
                                    .size(ButtonSize::Compact)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                            d.npm_environment.push((
                                                input("Name", "", window, cx),
                                                input("Value", "", window, cx),
                                            ));
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .into_any_element()
            } else {
                let live_edit = &draft.npm_browser_live_edit;
                let launch_browser = live_edit.launch_browser;
                v_flex()
                    .gap_3()
                    .w_full()
                    .child(
                        Switch::new(
                            "launch-browser-switch",
                            if launch_browser { ToggleState::Selected } else { ToggleState::Unselected },
                        )
                        .label("Launch Browser")
                        .label_position(SwitchLabelPosition::End)
                        .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                            if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                d.npm_browser_live_edit.launch_browser = state.selected();
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Browser").size(LabelSize::Small).color(Color::Muted))
                            .child(
                                Button::new(
                                    "browser-sel",
                                    if live_edit.browser.is_empty() { "Chrome" } else { &live_edit.browser },
                                )
                                .disabled(!launch_browser)
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("URL").size(LabelSize::Small).color(Color::Muted))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div().flex_1().child(
                                            Label::new(if live_edit.url.is_empty() { "http://localhost:3000" } else { &live_edit.url })
                                        )
                                    )
                                    .child(Button::new("browse-url", "Browse…").disabled(!launch_browser)),
                            ),
                    )
                    .child(
                        Switch::new(
                            "start-js-debugger",
                            if live_edit.start_debugger { ToggleState::Selected } else { ToggleState::Unselected },
                        )
                        .label("Start JavaScript debugger after launch")
                        .label_position(SwitchLabelPosition::End)
                        .disabled(!launch_browser)
                        .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                            if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                d.npm_browser_live_edit.start_debugger = state.selected();
                                cx.notify();
                            }
                        })),
                    )
                    .into_any_element()
            })
            .into_any_element()
    }

    fn render_before_launch_section(&self, draft: &ConfigurationDraft, cx: &mut Context<Self>) -> impl IntoElement {
        let expanded = draft.before_launch_expanded;
        v_flex()
            .gap_1()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .pt_2()
            .child(
                h_flex()
                    .id("before-launch-header")
                    .gap_1()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                            d.before_launch_expanded = !d.before_launch_expanded;
                            cx.notify();
                        }
                    }))
                    .child(
                        Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                            .size(IconSize::Small),
                    )
                    .child(Label::new("Before launch: activate tool window").size(LabelSize::Small)),
            )
            .when(expanded, |this| {
                this.child(
                    v_flex()
                        .pl_4()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    PopoverMenu::new("add-before-launch")
                                        .trigger(IconButton::new("add-bl-btn", IconName::Plus).icon_size(IconSize::XSmall))
                                        .menu({
                                            let editor = cx.weak_entity();
                                            move |window, cx| {
                                                let editor = editor.clone();
                                                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                                    let ed1 = editor.clone();
                                                    menu = menu.entry("Run npm script 'build'", None, move |_, cx| {
                                                        ed1.update(cx, |this, cx| {
                                                            if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                                                d.before_launch.push(BeforeLaunchTask::NpmScript {
                                                                    script: "build".into(),
                                                                    package_json: Some("package.json".into()),
                                                                });
                                                                cx.notify();
                                                            }
                                                        }).log_err();
                                                    });
                                                    let ed2 = editor.clone();
                                                    menu = menu.entry("Build Cargo target", None, move |_, cx| {
                                                        ed2.update(cx, |this, cx| {
                                                            if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                                                d.before_launch.push(BeforeLaunchTask::Build {
                                                                    name: "cargo build".into(),
                                                                    target: None,
                                                                });
                                                                cx.notify();
                                                            }
                                                        }).log_err();
                                                    });
                                                    menu
                                                }))
                                            }
                                        })
                                )
                                .child(
                                    IconButton::new("remove-bl-btn", IconName::Close)
                                        .icon_size(IconSize::XSmall)
                                        .disabled(draft.before_launch.is_empty())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                                d.before_launch.pop();
                                                cx.notify();
                                            }
                                        }))
                                )
                        )
                        .child(if draft.before_launch.is_empty() {
                            Label::new("There are no tasks to run before launch")
                                .color(Color::Muted)
                                .size(LabelSize::Small)
                                .into_any_element()
                        } else {
                            v_flex()
                                .gap_0p5()
                                .children(draft.before_launch.iter().enumerate().map(|(idx, task)| {
                                    h_flex()
                                        .gap_2()
                                        .child(Label::new(format!("{}. {}", idx + 1, task.label())).size(LabelSize::Small))
                                }))
                                .into_any_element()
                        }),
                )
            })
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).text(cx).to_lowercase();
        let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, draft) in self.drafts.iter().enumerate() {
            if draft.name.read(cx).text(cx).to_lowercase().contains(&query) {
                let key = if self.sort_alphabetical {
                    "All Configurations"
                } else {
                    draft.entry.configuration.kind()
                };
                groups.entry(key).or_default().push(index);
            }
        }

        v_flex()
            .w(px(235.))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .overflow_hidden()
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .child(
                // Sidebar Toolbar: +, -, Copy, Folder, Sort
                h_flex()
                    .p_1p5()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        PopoverMenu::new("add-config-popup")
                            .trigger(
                                IconButton::new("add-config-btn", IconName::Plus)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Add New Configuration"))
                            )
                            .menu({
                                let editor = cx.weak_entity();
                                move |window, cx| {
                                    let editor = editor.clone();
                                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                        for provider in registry::ConfigurationProvider::all() {
                                            if !matches!(
                                                provider.id,
                                                "npm"
                                                    | "node"
                                                    | "javascript_debug"
                                                    | "attach_node"
                                                    | "cargo"
                                            ) {
                                                continue;
                                            }
                                            let editor = editor.clone();
                                            let id = provider.id;
                                            menu = menu.entry(provider.display_name, None, move |window, cx| {
                                                editor.update(cx, |this, cx| {
                                                    this.add(id, window, cx);
                                                }).log_err();
                                            });
                                        }
                                        menu
                                    }))
                                }
                            })
                    )
                    .child(
                        IconButton::new("remove-config-btn", IconName::Close)
                            .icon_size(IconSize::Small)
                            .disabled(self.selected.is_none())
                            .tooltip(Tooltip::text("Remove Configuration"))
                            .on_click(cx.listener(|this, _, window, cx| this.remove(window, cx)))
                    )
                    .child(
                        IconButton::new("copy-config-btn", IconName::Copy)
                            .icon_size(IconSize::Small)
                            .disabled(self.selected.is_none())
                            .tooltip(Tooltip::text("Copy Configuration"))
                            .on_click(cx.listener(|this, _, window, cx| this.duplicate(window, cx)))
                    )
                    .child(
                        IconButton::new("folder-config-btn", IconName::Folder)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Move into folder"))
                    )
                    .child(
                        IconButton::new("sort-config-btn", IconName::ArrowDown)
                            .icon_size(IconSize::Small)
                            .toggle_state(self.sort_alphabetical)
                            .tooltip(Tooltip::text("Sort Alphabetically"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sort_alphabetical = !this.sort_alphabetical;
                                cx.notify();
                            }))
                    )
            )
            .child(
                div()
                    .px_1p5()
                    .py_1()
                    .child(self.search.clone())
            )
            .child(
                div()
                    .id("config-tree-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1p5()
                    .gap_2()
                    .children(groups.into_iter().map(|(kind, indices)| {
                        v_flex()
                            .gap_0p5()
                            .child(Label::new(kind).size(LabelSize::XSmall).color(Color::Muted))
                            .children(indices.into_iter().filter_map(|index| {
                                let draft = self.drafts.get(index)?;
                                let is_sel = self.selected == Some(index);
                                let name = draft.name.read(cx).text(cx);
                                let icon = draft.entry.configuration.icon();
                                Some(
                                    h_flex()
                                        .id(("cfg-tree-item", index))
                                        .px_2()
                                        .py_1()
                                        .gap_1p5()
                                        .rounded_sm()
                                        .cursor_pointer()
                                        .when(is_sel, |this| {
                                            this.bg(cx.theme().colors().element_selected)
                                        })
                                        .when(!is_sel, |this| {
                                            this.hover(|h| h.bg(cx.theme().colors().element_hover))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.selected = Some(index);
                                            this.error = None;
                                            cx.notify();
                                        }))
                                        .child(Icon::new(icon).size(IconSize::Small).color(if is_sel { Color::Default } else { Color::Muted }))
                                        .child(Label::new(name).size(LabelSize::Small).color(if is_sel { Color::Default } else { Color::Muted }))
                                )
                            }))
                    }))
            )
            .child(
                h_flex()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        Button::new("edit-templates-link", "Configuration templates…")
                            .size(ButtonSize::Compact)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing_templates = !this.editing_templates;
                                cx.notify();
                            }))
                    )
            )
    }

    fn render_right_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let draft = self.selected.and_then(|idx| self.drafts.get(idx));

        v_flex()
            .id("config-editor-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_y_scroll()
            .p_3()
            .gap_3()
            .when_some(draft, |this, draft| {
                let is_npm = draft.is_npm();
                this.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .gap_2()
                        .child(draft.name.clone())
                        .child(
                            h_flex()
                                .w_full()
                                .flex_wrap()
                                .gap_4()
                                .items_center()
                                .child(
                                    Switch::new(
                                        "enable-debug-switch",
                                        if draft.debug_enabled { ToggleState::Selected } else { ToggleState::Unselected },
                                    )
                                    .label("Debug enabled")
                                    .label_position(SwitchLabelPosition::End)
                                    .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                                        if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                            d.debug_enabled = state.selected();
                                            cx.notify();
                                        }
                                    })),
                                )
                                .child(
                                    Switch::new(
                                        "multiple-instances-switch",
                                        if draft.allow_multiple_instances { ToggleState::Selected } else { ToggleState::Unselected },
                                    )
                                    .label("Allow multiple instances")
                                    .label_position(SwitchLabelPosition::End)
                                    .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                                        if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                            d.allow_multiple_instances = state.selected();
                                            cx.notify();
                                        }
                                    })),
                                )
                                .child(
                                    Switch::new(
                                        "store-project-file-switch",
                                        if draft.entry.key.storage == Storage::Project { ToggleState::Selected } else { ToggleState::Unselected },
                                    )
                                    .label("Store as project file")
                                    .label_position(SwitchLabelPosition::End)
                                    .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                                        if let Some(d) = this.selected.and_then(|i| this.drafts.get_mut(i)) {
                                            d.entry.key.storage = if state.selected() { Storage::Project } else { Storage::Local };
                                            cx.notify();
                                        }
                                    })),
                                ),
                        ),
                )
                .child(if is_npm {
                    self.render_npm_form(draft, cx)
                } else {
                    let is_cargo = draft.builds_cargo_target();
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .gap_2()
                        .when(
                            is_cargo
                                || !draft.debug_enabled
                                || !draft.run.program.read(cx).text(cx).is_empty(),
                            |form| {
                                form.child(draft.run.program.clone())
                                    .child(draft.run.cwd.clone())
                            })
                            .when(draft.debug_enabled, |form| {
                                let adapter_text = draft.adapter.read(cx).text(cx);
                                let adapter_name = if adapter_text.is_empty() {
                                    "CodeLLDB".to_string()
                                } else {
                                    adapter_text
                                };
                                form.child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(Label::new("Debugger:").color(Color::Muted).size(LabelSize::Small))
                                        .child(Label::new(adapter_name).size(LabelSize::Small))
                                )
                                .when(!is_cargo, |form| {
                                    form.child(draft.debug.program.clone())
                                        .when(draft.run.program.read(cx).text(cx).is_empty(), |form| {
                                            form.child(draft.debug.cwd.clone())
                                        })
                                })
                            })
                            .into_any_element()
                    }
                )
                .child(self.render_before_launch_section(draft, cx))
            })
            .when(draft.is_none(), |this| {
                this.flex().items_center().justify_center().child(
                    Label::new("Select or add a configuration to edit its settings")
                        .color(Color::Muted)
                )
            })
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dirty = self.has_changes(cx);
        let draft = self.selected.and_then(|i| self.drafts.get(i));
        let can_run = draft.is_some();
        let can_debug = draft.is_some_and(|draft| draft.debug_enabled) && !self.busy;

        h_flex()
            .flex_shrink_0()
            .p_2p5()
            .gap_2()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .child(
                Button::new("help-btn", "Help")
                    .size(ButtonSize::Compact)
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        PopoverMenu::new("run-split-btn")
                            .trigger(
                                Button::new("run-modal-btn", "Run")
                                    .size(ButtonSize::Compact)
                                    .start_icon(Icon::new(IconName::PlayFilled).color(Color::Success))
                                    .disabled(self.busy || !can_run)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save(true, Some(false), window, cx);
                                    }))
                            )
                            .menu({
                                let editor = cx.weak_entity();
                                move |window, cx| {
                                    let editor = editor.clone();
                                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                        let ed = editor.clone();
                                        if can_debug {
                                            menu = menu.entry("Debug", None, move |window, cx| {
                                                ed.update(cx, |this, cx| {
                                                    this.save(true, Some(true), window, cx);
                                                })
                                                .log_err();
                                            });
                                        }
                                        menu = menu.entry(
                                            "Run with Coverage (unavailable)",
                                            None,
                                            |_window, _| {},
                                        );
                                        menu
                                    }))
                                }
                            })
                    )
                    .child(
                        Button::new("ok-configurations", "OK")
                            .size(ButtonSize::Compact)
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save(true, None, window, cx);
                            })),
                    )
                    .child(
                        Button::new("cancel-configurations", "Cancel")
                            .size(ButtonSize::Compact)
                            .disabled(self.busy)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new("apply-configuration", "Apply")
                            .size(ButtonSize::Compact)
                            .disabled(self.busy || !is_dirty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save(false, None, window, cx);
                            })),
                    ),
            )
    }
}

impl Render for ConfigurationEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("run-configurations-editor")
            .key_context("RunConfigurations")
            .track_focus(&self.focus_handle)
            .w(px(800.).min(window.viewport_size().width - px(32.)))
            .h(px(720.).min(window.viewport_size().height - px(48.)))
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_lg()
            .overflow_hidden()
            .shadow_lg()
            .child(
                // Header
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new("Run/Debug Configurations").size(LabelSize::Large)),
            )
            .child(
                // Main content: left sidebar (235px) + right form pane
                h_flex()
                    .flex_1()
                    .w_full()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_sidebar(cx))
                    .child(self.render_right_pane(cx)),
            )
            .when_some(self.error.clone(), |form, error| {
                form.child(
                    div()
                        .px_3()
                        .py_1p5()
                        .bg(cx.theme().status().error_background)
                        .child(Label::new(error).color(Color::Error).size(LabelSize::Small)),
                )
            })
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn debugger_toggle_round_trips_configuration_editor(cx: &mut gpui::TestAppContext) {
        crate::tests::init_test(cx);
        let fs = fs::FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            Path::new("/toggle-project"),
            json!({ "package.json": "{}" }),
        )
        .await;
        let project = Project::test(fs, [Path::new("/toggle-project")], cx).await;
        let workspace = crate::tests::init_test_workspace(&project, cx).await;
        workspace
            .update(cx, |_, window, cx| {
                let worktree_id = project
                    .read(cx)
                    .worktrees(cx)
                    .next()
                    .expect("Project worktree")
                    .read(cx)
                    .id();
                for kind in ["npm", "node", "javascript_debug", "attach_node", "cargo"] {
                    let root = PathBuf::from("/toggle-project");
                    let entry = ConfigurationEntry {
                        key: ConfigurationKey {
                            root: root.clone(),
                            storage: Storage::Project,
                            id: kind.into(),
                        },
                        worktree_id,
                        configuration: registry::ConfigurationProvider::create_default(
                            kind, &root, None,
                        ),
                    };
                    let mut draft = ConfigurationDraft::new(entry.clone(), false, window, cx);
                    assert!(!draft.debug_enabled);
                    draft.debug_enabled = true;
                    let enabled = draft.configuration(cx).expect("Enable debugger");
                    let mut reopened = ConfigurationDraft::new(
                        ConfigurationEntry {
                            configuration: enabled,
                            ..entry.clone()
                        },
                        true,
                        window,
                        cx,
                    );
                    assert!(reopened.debug_enabled);
                    let debug_settings = reopened.entry.configuration.debug.clone();
                    reopened.debug_enabled = false;
                    let disabled = reopened.configuration(cx).expect("Disable debugger");
                    assert!(!disabled.debug_enabled);
                    assert_eq!(disabled.debug, debug_settings);
                    let reopened = ConfigurationDraft::new(
                        ConfigurationEntry {
                            configuration: disabled,
                            ..entry
                        },
                        true,
                        window,
                        cx,
                    );
                    assert!(!reopened.debug_enabled);
                }
            })
            .expect("Configuration editor window");
    }

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
    fn run_configurations_build_cargo_binary_before_debugging() -> Result<()> {
        let root = std::env::temp_dir().join(format!("run-defaults-{}", Uuid::new_v4()));
        let configuration =
            cargo_configuration(&metadata(&root, &root.join("custom-output")), &root)
                .context("Expected a configuration before any executable exists")?;
        let reopened: RunConfiguration =
            serde_json::from_str(&serde_json::to_string(&configuration)?)?;
        assert_eq!(reopened, configuration);
        configuration.validate()?;
        let debug = configuration.debug.context("Expected a debug target")?;
        assert_eq!(debug.adapter.as_ref(), "CodeLLDB");
        assert!(debug.config.get("program").is_none());
        assert_eq!(debug.config["cwd"], json!(root));
        assert_eq!(debug.config["request"], "launch");
        let Some(BuildTaskDefinition::Template {
            task_template,
            locator_name,
        }) = &debug.build
        else {
            panic!("Expected a task template build target");
        };
        assert_eq!(locator_name.as_deref(), Some("rust-cargo-locator"));
        assert_eq!(task_template.command, "cargo");
        assert_eq!(
            task_template.args,
            ["build", "--package", "application", "--bin", "application"]
        );
        Ok(())
    }

    #[test]
    fn run_configurations_do_not_choose_ambiguous_cargo_targets() {
        let root = Path::new("/run-defaults-root");
        let mut metadata = metadata(root, &root.join("target"));
        metadata["packages"][0]["default_run"] = Value::Null;
        assert!(cargo_configuration(&metadata, root).is_none());
        metadata["packages"][0]["targets"] = json!([{"name": "application", "kind": ["lib"]}]);
        assert!(cargo_configuration(&metadata, root).is_none());
    }

    #[test]
    fn run_configurations_prefer_open_package_over_workspace_defaults() -> Result<()> {
        let root = std::env::temp_dir().join(format!("run-defaults-{}", Uuid::new_v4()));
        let configuration = cargo_configuration(
            &metadata(&root, &root.join("target")),
            &root.join("other"),
        )
        .context("Expected a configuration for the open package root")?;
        assert_eq!(configuration.name, "other");
        assert_eq!(
            configuration
                .run
                .as_ref()
                .map(|run| run.args.as_slice()),
            Some(["run".to_owned(), "--package".into(), "other".into(), "--bin".into(), "other".into()].as_slice())
        );
        Ok(())
    }
    #[test]
    fn test_cargo_provider_creates_debug_disabled_default() {
        let root = Path::new("/workspace");
        let config = registry::ConfigurationProvider::create_default("cargo", root, None);
        assert_eq!(config.name, "Cargo");
        assert!(!config.debug_enabled);
        assert_eq!(config.configuration_type.as_deref(), Some("cargo"));
        assert!(config.run.is_some());
        assert!(config.debug.is_some());
        let debug = config.debug.unwrap();
        assert_eq!(debug.adapter.as_ref(), "CodeLLDB");
    }
    #[test]
    fn test_debug_configuration_validation_and_adapter_fallback() {
        let root = Path::new("/workspace");
        let mut config =
            registry::ConfigurationProvider::create_default("javascript_debug", root, None);
        config.debug_enabled = true;
        assert!(config.run.is_none());
        assert!(config.debug.is_some());
        assert!(config.validate().is_ok());

        config.debug.as_mut().unwrap().adapter = "".into();
        assert!(config.validate().is_err());
    }
}
