use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use extension::{ExtensionHostProxy, ExtensionThemeProxy};
use fs::Fs;
use gpui::{App, BackgroundExecutor, SharedString, Task};
use theme::{ThemeRegistry, deserialize_icon_theme};

pub fn init(
    extension_host_proxy: Arc<ExtensionHostProxy>,
    theme_registry: Arc<ThemeRegistry>,
    executor: BackgroundExecutor,
) {
    extension_host_proxy.register_theme_proxy(ThemeRegistryProxy {
        theme_registry,
        executor,
    });
}

struct ThemeRegistryProxy {
    theme_registry: Arc<ThemeRegistry>,
    executor: BackgroundExecutor,
}

impl ExtensionThemeProxy for ThemeRegistryProxy {
    fn set_extensions_loaded(&self) {
        self.theme_registry.set_extensions_loaded();
    }

    fn list_theme_names(&self, _theme_path: PathBuf, _fs: Arc<dyn Fs>) -> Task<Result<Vec<String>>> {
        Task::ready(Ok(Vec::new()))
    }

    fn remove_user_themes(&self, _themes: Vec<SharedString>) {}

    fn load_user_theme(&self, _theme_path: PathBuf, _fs: Arc<dyn Fs>) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn reload_current_theme(&self, _cx: &mut App) {}

    fn list_icon_theme_names(
        &self,
        icon_theme_path: PathBuf,
        fs: Arc<dyn Fs>,
    ) -> Task<Result<Vec<String>>> {
        self.executor.spawn(async move {
            let icon_theme_family =
                deserialize_icon_theme(&fs.load_bytes(&icon_theme_path).await?)?;
            Ok(icon_theme_family
                .themes
                .into_iter()
                .map(|theme| theme.name)
                .collect())
        })
    }

    fn remove_icon_themes(&self, icon_themes: Vec<SharedString>) {
        self.theme_registry.remove_icon_themes(&icon_themes);
    }

    fn load_icon_theme(
        &self,
        icon_theme_path: PathBuf,
        icons_root_dir: PathBuf,
        fs: Arc<dyn Fs>,
    ) -> Task<Result<()>> {
        let theme_registry = self.theme_registry.clone();
        self.executor.spawn(async move {
            let icon_theme_family =
                deserialize_icon_theme(&fs.load_bytes(&icon_theme_path).await?)?;
            theme_registry.load_icon_theme(icon_theme_family, &icons_root_dir)
        })
    }

    fn reload_current_icon_theme(&self, cx: &mut App) {
        theme_settings::reload_icon_theme(cx)
    }
}
