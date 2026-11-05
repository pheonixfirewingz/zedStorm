use std::{env, fs, path::Path};

use serde::Deserialize;
use zed::lsp::{Completion, CompletionKind};
use zed::settings::LspSettings;
use zed::CodeLabelSpan;
use zed_extension_api::serde_json::json;
use zed_extension_api::{
    self as zed, serde_json, Command, LanguageServerId, Result, SlashCommand,
    SlashCommandArgumentCompletion, SlashCommandOutput, SlashCommandOutputSection, Worktree,
};

const SERVER_PACKAGE: &str = "@angular/language-server";
const SERVER_REL_PATH: &str = "node_modules/@angular/language-server/index.js";
const LANGUAGE_SERVICE_PACKAGE: &str = "@angular/language-service";
const TYPESCRIPT_PACKAGE: &str = "typescript";
const BUNDLED_TYPESCRIPT_VERSION: &str = "5.8.2";

#[derive(Debug, Deserialize, Default)]
struct UserSettings {
    /// Maximum heap size (in MB) for the language server process, passed to node as `--max-old-space-size`.
    max_ts_server_memory: Option<u32>,
    /// Override the location of the `@angular/language-server` package or entry script.
    angular_language_server_path: Option<String>,
    /// Additional probe locations for Angular and TypeScript.
    probe_locations: Option<Vec<String>>,
    /// Include automatic optional chaining in suggestions.
    include_automatic_optional_chaining: Option<bool>,
    /// Log verbosity level ("normal", "verbose", "off").
    log_verbosity: Option<String>,
}

struct AngularExtension {
    cached_server_path: Option<String>,
}

fn extension_install_dir() -> String {
    env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .replace('\\', "/")
}

impl AngularExtension {
    fn normalize_path(path: &str) -> String {
        let p = path.trim().replace('\\', "/");
        let p = p.strip_suffix("/index.js").unwrap_or(&p);
        p.trim_end_matches('/').to_string()
    }

    fn expand_home(worktree: &Worktree, path: &str) -> String {
        let rest = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"));

        match rest {
            Some(rest) => {
                let env = worktree.shell_env();
                let home = env
                    .iter()
                    .find(|(k, _)| k == "HOME" || k == "USERPROFILE")
                    .map(|(_, v)| v.as_str());

                match home {
                    Some(home) => {
                        let home = home.trim_end_matches(['/', '\\']).replace('\\', "/");
                        format!("{home}/{rest}")
                    }
                    None => path.to_string(),
                }
            }
            None => path.to_string(),
        }
    }

    /// Checks if a file exists on the filesystem.
    fn file_exists(path: &str) -> bool {
        fs::metadata(path).is_ok_and(|meta| meta.is_file())
    }

    /// Checks if a directory exists on the filesystem.
    fn dir_exists(path: &str) -> bool {
        fs::metadata(path).is_ok_and(|meta| meta.is_dir())
    }

    /// Find or auto-install the Angular Language Server.
    fn resolve_server_script(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
        override_path: Option<&str>,
    ) -> Result<String> {
        // 1. If user supplied an explicit override path, use it.
        if let Some(user_path) = override_path {
            let expanded = Self::expand_home(worktree, user_path);
            let normalized = Self::normalize_path(&expanded);
            if !normalized.is_empty() {
                let candidate = if normalized.ends_with(".js") {
                    normalized
                } else {
                    format!("{normalized}/index.js")
                };
                if Self::file_exists(&candidate) {
                    return Ok(candidate);
                }
            }
        }

        // 2. If already resolved and verified, reuse it.
        if let Some(cached) = &self.cached_server_path {
            if Self::file_exists(cached) {
                return Ok(cached.clone());
            }
        }

        // 3. Check inside the worktree's local node_modules
        let root = worktree.root_path().replace('\\', "/");
        let project_server = format!("{root}/node_modules/@angular/language-server/index.js");
        if Self::file_exists(&project_server) {
            self.cached_server_path = Some(project_server.clone());
            return Ok(project_server);
        }

        // Check if `ngserver` binary is available on PATH
        if let Some(ngserver_bin) = worktree.which("ngserver") {
            self.cached_server_path = Some(ngserver_bin.clone());
            return Ok(ngserver_bin);
        }

        // 4. Fallback to extension's managed node_modules
        let ext_dir = extension_install_dir();
        let bundled_path = format!("{ext_dir}/{SERVER_REL_PATH}");
        if Self::file_exists(&bundled_path) {
            self.cached_server_path = Some(bundled_path.clone());
            return Ok(bundled_path);
        }

        // 5. Automatically install @angular/language-server and typescript
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let latest_version = zed::npm_package_latest_version(SERVER_PACKAGE)?;
        let installed_version = zed::npm_package_installed_version(SERVER_PACKAGE)?;

        if !Self::file_exists(&bundled_path) || installed_version.as_ref() != Some(&latest_version)
        {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            // Install @angular/language-server
            zed::npm_install_package(SERVER_PACKAGE, &latest_version)?;

            // Install matching @angular/language-service and compatible typescript
            let _ = zed::npm_install_package(LANGUAGE_SERVICE_PACKAGE, &latest_version);
            let _ = zed::npm_install_package(TYPESCRIPT_PACKAGE, BUNDLED_TYPESCRIPT_VERSION);

            if !Self::file_exists(&bundled_path) {
                return Err(format!(
                    "Installed package '{SERVER_PACKAGE}' did not contain expected entry '{bundled_path}'",
                ));
            }
        }

        self.cached_server_path = Some(bundled_path.clone());
        Ok(bundled_path)
    }

    /// Compute probe directories for TypeScript and Angular Language Service.
    fn probe_locations(
        worktree: &Worktree,
        server_path: &str,
        extra_probes: Option<&[String]>,
    ) -> String {
        let root = worktree.root_path().replace('\\', "/");
        let mut paths = Vec::new();

        // 1. Workspace root & its node_modules
        paths.push(root.clone());
        paths.push(format!("{root}/node_modules"));

        // 2. Check parent directories up to 3 levels for monorepo / workspaces
        let mut current = Path::new(&root);
        for _ in 0..3 {
            if let Some(parent) = current.parent() {
                let p_str = parent.to_string_lossy().replace('\\', "/");
                if !p_str.is_empty() {
                    let parent_nm = format!("{p_str}/node_modules");
                    if Self::dir_exists(&parent_nm) {
                        paths.push(parent_nm);
                    }
                    paths.push(p_str);
                    current = parent;
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // 3. Add user custom probes if provided
        if let Some(extras) = extra_probes {
            for probe in extras {
                let exp = Self::expand_home(worktree, probe);
                let norm = Self::normalize_path(&exp);
                if !norm.is_empty() {
                    paths.push(norm);
                }
            }
        }

        // 4. Server directory & extension install directory as fallback
        let ext_dir = extension_install_dir();
        paths.push(format!("{ext_dir}/node_modules"));
        paths.push(ext_dir);

        // Also add the directory hosting the resolved server script
        if let Some((server_dir, _)) = server_path.rsplit_once('/') {
            paths.push(server_dir.to_string());
            if let Some((parent_dir, _)) = server_dir.rsplit_once('/') {
                paths.push(parent_dir.to_string());
            }
        }

        // Deduplicate while preserving priority order
        let mut unique = Vec::with_capacity(paths.len());
        for p in paths {
            if !unique.contains(&p) {
                unique.push(p);
            }
        }

        unique.join(",")
    }
}

impl zed::Extension for AngularExtension {
    fn new() -> Self {
        Self {
            cached_server_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let settings: UserSettings =
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .ok()
                .and_then(|s| s.initialization_options)
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| format!("Failed to parse `lsp.angular.initialization_options`: {e}"))?
                .unwrap_or_default();

        let server_script = self.resolve_server_script(
            language_server_id,
            worktree,
            settings.angular_language_server_path.as_deref(),
        )?;

        let probes = Self::probe_locations(
            worktree,
            &server_script,
            settings.probe_locations.as_deref(),
        );

        let mut args = Vec::new();

        // Node memory optimization flag
        if let Some(mb) = settings.max_ts_server_memory {
            args.push(format!("--max-old-space-size={mb}"));
        }

        // Pass server entry script
        args.push(server_script);
        args.push("--stdio".to_string());
        args.push("--tsProbeLocations".to_string());
        args.push(probes.clone());
        args.push("--ngProbeLocations".to_string());
        args.push(probes);
        args.push("--logToConsole".to_string());
        args.push("--logVerbosity".to_string());
        args.push(settings.log_verbosity.unwrap_or_else(|| "normal".to_string()));

        Ok(Command {
            command: zed::node_binary_path()?,
            args,
            env: worktree.shell_env(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let settings: UserSettings =
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .ok()
                .and_then(|s| s.initialization_options)
                .map(serde_json::from_value)
                .transpose()
                .unwrap_or_default()
                .unwrap_or_default();

        let user_opts = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|s| s.initialization_options);

        let default_opts = json!({
            "includeAutomaticOptionalChaining": settings.include_automatic_optional_chaining.unwrap_or(true),
            "includeCompletionsForModuleExports": true,
            "forceStrictTemplates": true,
        });

        match user_opts {
            Some(mut custom) => {
                if let (Some(cust_obj), Some(def_obj)) = (custom.as_object_mut(), default_opts.as_object()) {
                    for (k, v) in def_obj {
                        if !cust_obj.contains_key(k) {
                            cust_obj.insert(k.clone(), v.clone());
                        }
                    }
                }
                Ok(Some(custom))
            }
            None => Ok(Some(default_opts)),
        }
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let settings: UserSettings =
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .ok()
                .and_then(|s| s.initialization_options)
                .map(serde_json::from_value)
                .transpose()
                .unwrap_or_default()
                .unwrap_or_default();

        let user_settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|s| s.settings);

        let default_settings = json!({
            "angular": {
                "suggest": {
                    "includeAutomaticOptionalChaining": settings.include_automatic_optional_chaining.unwrap_or(true),
                    "includeCompletionsWithSnippetText": true
                },
                "inlayHints": {
                    "missingProps": true,
                    "bindingTypes": true
                }
            }
        });

        match user_settings {
            Some(mut custom) => {
                if let (Some(cust_obj), Some(def_obj)) = (custom.as_object_mut(), default_settings.as_object()) {
                    for (k, v) in def_obj {
                        if !cust_obj.contains_key(k) {
                            cust_obj.insert(k.clone(), v.clone());
                        }
                    }
                }
                Ok(Some(custom))
            }
            None => Ok(Some(default_settings)),
        }
    }

    fn language_server_additional_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        target_language_server_id: &LanguageServerId,
        _worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        match target_language_server_id.as_ref() {
            "typescript-language-server" => Ok(Some(json!({
                "plugins": [{
                    "name": "@angular/language-service",
                    "location": extension_install_dir(),
                    "languages": ["typescript", "angular"],
                }],
            }))),
            _ => Ok(None),
        }
    }

    fn language_server_additional_workspace_configuration(
        &mut self,
        _language_server_id: &LanguageServerId,
        target_language_server_id: &LanguageServerId,
        _worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        match target_language_server_id.as_ref() {
            "vtsls" => Ok(Some(json!({
                "vtsls": {
                    "tsserver": {
                        "globalPlugins": [{
                            "name": "@angular/language-service",
                            "location": extension_install_dir(),
                            "enableForWorkspaceTypeScriptVersions": true,
                            "languages": ["angular", "typescript"],
                            "configNamespace": "typescript"
                        }]
                    }
                },
            }))),
            _ => Ok(None),
        }
    }

    fn label_for_completion(
        &self,
        _language_server_id: &LanguageServerId,
        completion: Completion,
    ) -> Option<zed::CodeLabel> {
        let highlight_name = match completion.kind? {
            CompletionKind::Class | CompletionKind::Interface => "type",
            CompletionKind::Constructor => "constructor",
            CompletionKind::Constant => "constant",
            CompletionKind::Function | CompletionKind::Method => "function",
            CompletionKind::Property | CompletionKind::Field => "property",
            CompletionKind::Variable => "variable",
            CompletionKind::Keyword => "keyword",
            CompletionKind::Enum => "enum",
            CompletionKind::Module => "module",
            CompletionKind::Snippet => "keyword",
            CompletionKind::Value => "tag",
            _ => return None,
        };

        let len = completion.label.len();
        let name_span = CodeLabelSpan::literal(completion.label, Some(highlight_name.to_string()));

        let spans = match completion.detail {
            Some(detail) => vec![
                name_span,
                CodeLabelSpan::literal(" ", None),
                CodeLabelSpan::literal(detail, Some("comment".to_string())),
            ],
            None => vec![name_span],
        };

        Some(zed::CodeLabel {
            code: Default::default(),
            spans,
            filter_range: (0..len).into(),
        })
    }

    fn complete_slash_command_argument(
        &self,
        _command: SlashCommand,
        _args: Vec<String>,
    ) -> Result<Vec<SlashCommandArgumentCompletion>, String> {
        Ok(vec![
            SlashCommandArgumentCompletion {
                label: "cli".to_string(),
                new_text: "cli".to_string(),
                run_command: true,
            },
            SlashCommandArgumentCompletion {
                label: "components".to_string(),
                new_text: "components".to_string(),
                run_command: true,
            },
            SlashCommandArgumentCompletion {
                label: "control-flow".to_string(),
                new_text: "control-flow".to_string(),
                run_command: true,
            },
            SlashCommandArgumentCompletion {
                label: "signals".to_string(),
                new_text: "signals".to_string(),
                run_command: true,
            },
        ])
    }

    fn run_slash_command(
        &self,
        _command: SlashCommand,
        args: Vec<String>,
        _worktree: Option<&Worktree>,
    ) -> Result<SlashCommandOutput, String> {
        let topic = args.first().map(|s| s.as_str()).unwrap_or("help");

        let text = match topic {
            "cli" => r#"### Angular CLI Commands Quick Reference
- `ng generate component <name>` (or `ng g c <name>`) - Generate a standalone component
- `ng generate service <name>` (or `ng g s <name>`) - Generate an `@Injectable` service
- `ng generate directive <name>` (or `ng g d <name>`) - Generate a directive
- `ng generate pipe <name>` (or `ng g p <name>`) - Generate a pipe
- `ng generate guard <name>` (or `ng g g <name>`) - Generate a functional route guard
- `ng serve` - Start development server
- `ng test` - Run unit tests via Karma / Web Test Runner / Jest
- `ng build` - Build production bundle
"#.to_string(),
            "components" => r#"### WebStorm Angular Component Conventions
In Angular projects, components are typically split across 4 related files:
1. `name.component.ts`: Component logic and class metadata
2. `name.component.html`: Component template (Angular HTML)
3. `name.component.css` or `.scss`: Scoped component styling
4. `name.component.spec.ts`: Component unit tests

**Modern Standalone Component Pattern:**
```typescript
import { Component, ChangeDetectionStrategy, signal } from '@angular/core';

@Component({
  selector: 'app-example',
  standalone: true,
  imports: [],
  templateUrl: './example.component.html',
  styleUrls: ['./example.component.scss'],
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ExampleComponent {
  readonly count = signal(0);
}
```
"#.to_string(),
            "control-flow" => r#"### Modern Angular Control Flow (@if, @for, @switch, @defer)
Angular 17+ introduced built-in template control flow:

**1. Conditional (@if / @else if / @else):**
```html
@if (isLoggedIn()) {
  <user-profile />
} @else if (isLoading()) {
  <loading-spinner />
} @else {
  <login-form />
}
```

**2. Loops (@for with mandatory track):**
```html
@for (item of items(); track item.id) {
  <item-card [item]="item" />
} @empty {
  <p>No items found.</p>
}
```

**3. Pattern Matching (@switch / @case / @default):**
```html
@switch (status()) {
  @case ('active') { <status-badge color="green" /> }
  @case ('pending') { <status-badge color="amber" /> }
  @default { <status-badge color="gray" /> }
}
```

**4. Deferred Loading (@defer):**
```html
@defer (on viewport) {
  <heavy-chart [data]="chartData()" />
} @placeholder {
  <div class="skeleton">Loading chart preview...</div>
} @loading (minimum 500ms) {
  <loading-spinner />
} @error {
  <p>Failed to load chart.</p>
}
```

**5. Template Variables (@let):**
```html
@let total = subtotal() + tax();
<p>Total: {{ total | currency }}</p>
```
"#.to_string(),
            "signals" => r#"### Modern Angular Signals & Reactivity Cheat Sheet
- **Signal**: `const count = signal(0);` (read: `count()`, write: `count.set(1)`, update: `count.update(n => n + 1)`)
- **Computed**: `const double = computed(() => count() * 2);`
- **Effect**: `effect(() => console.log('Count is', count()));`
- **Signal Input**: `readonly user = input<User>();` or `readonly title = input.required<string>();`
- **Signal Output**: `readonly select = output<Item>();`
- **Two-way Model**: `readonly checked = model<boolean>(false);`
"#.to_string(),
            _ => r#"### Angular Extension for ZedStorm
This extension provides a WebStorm-grade Angular development experience:
- **Language Server**: Autocompletion, diagnostics, go to definition, and template type checking for both HTML and TypeScript.
- **Tree-sitter Grammar**: Full syntax highlighting for templates, bindings `[...]`, `(...)`, `[(...)]`, `*directives`, and control flow (`@if`, `@for`, `@switch`, `@defer`, `@let`).
- **Live Templates / Snippets**: Predefined snippets for components, services, pipes, directives, guards, signals, and control flow blocks.
- **Slash Commands**:
  - `/angular cli` - Angular CLI commands & schematics cheat sheet
  - `/angular components` - Component structure & related files
  - `/angular control-flow` - Control flow syntax reference
  - `/angular signals` - Reactive signals & inputs reference
"#.to_string(),
        };

        let len = text.len();
        Ok(SlashCommandOutput {
            text,
            sections: vec![SlashCommandOutputSection {
                range: (0..len).into(),
                label: "Angular Helper".to_string(),
            }],
        })
    }
}

zed::register_extension!(AngularExtension);
