use anyhow::Result;
use clock::SystemClock;
use gpui::{App, Task};
use http_client::HttpClientWithUrl;
use release_channel::ReleaseChannel;
use std::sync::Arc;
use worktree::{UpdatedEntriesSet, WorktreeId};
#[cfg(target_os = "macos")]
use {regex::Regex, std::sync::LazyLock};

// Shared editor and extension APIs still refer to this type, but ZedStorm
// must never create an event collector or upload reports, even with old settings.
pub struct Telemetry;

pub fn should_install_crash_handler(_channel: ReleaseChannel) -> bool {
    true
}

pub fn os_name() -> String {
    #[cfg(target_os = "macos")]
    {
        "macOS".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        format!("Linux {}", gpui::guess_compositor())
    }
    #[cfg(target_os = "freebsd")]
    {
        format!("FreeBSD {}", gpui::guess_compositor())
    }

    #[cfg(target_os = "windows")]
    {
        "Windows".to_string()
    }
}

/// Note: This might do blocking IO! Only call from background threads
pub fn os_version() -> String {
    cfg_select! {
       feature = "test-support" => {
           // MacOS branch in particular is quite slow, hence we ought to "avoid" it in tests.
           "test binary".to_owned()
       }
       target_os = "macos" => {
           static MACOS_VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
               Regex::new(r"(\s*\(Build [^)]*[0-9]\))").unwrap()
           });
           use objc2_foundation::NSProcessInfo;
           let process_info = NSProcessInfo::processInfo();
           let version_nsstring = process_info.operatingSystemVersionString();
           // "Version 15.6.1 (Build 24G90)" -> "15.6.1 (Build 24G90)"
           let version_string = version_nsstring.to_string().replace("Version ", "");
           // "15.6.1 (Build 24G90)" -> "15.6.1"
           // "26.0.0 (Build 25A5349a)" -> unchanged (Beta or Rapid Security Response; ends with letter)
           MACOS_VERSION_REGEX
               .replace_all(&version_string, "")
               .to_string()
       }
       any(target_os = "linux", target_os = "freebsd") => {
           use std::path::Path;

           let content = if let Ok(file) = std::fs::read_to_string(&Path::new("/etc/os-release")) {
               file
           } else if let Ok(file) = std::fs::read_to_string(&Path::new("/usr/lib/os-release")) {
               file
           } else if let Ok(file) = std::fs::read_to_string(&Path::new("/var/run/os-release")) {
               file
           } else {
               log::error!(
                   "Failed to load /etc/os-release, /usr/lib/os-release, or /var/run/os-release"
               );
               "".to_string()
           };
           util::parse_os_release(&content).unwrap_or_else(|| "unknown".to_string())
       }
       target_os = "windows" => {
           let mut info = unsafe { std::mem::zeroed() };
           let status = unsafe { windows::Wdk::System::SystemServices::RtlGetVersion(&mut info) };
           if status.is_ok() {
               semver::Version::new(
                   info.dwMajorVersion as _,
                   info.dwMinorVersion as _,
                   info.dwBuildNumber as _,
               )
               .to_string()
           } else {
               "unknown".to_string()
           }
       }
    }
}

impl Telemetry {
    pub fn new(
        _clock: Arc<dyn SystemClock>,
        _client: Arc<HttpClientWithUrl>,
        _cx: &mut App,
    ) -> Arc<Self> {
        Arc::new(Self)
    }

    pub fn start(
        self: &Arc<Self>,
        _system_id: Option<String>,
        _installation_id: Option<String>,
        _session_id: String,
        _cx: &App,
    ) {
    }

    pub fn metrics_enabled(&self) -> bool {
        false
    }
    pub fn diagnostics_enabled(&self) -> bool {
        false
    }
    pub fn metrics_id(&self) -> Option<Arc<str>> {
        None
    }
    pub fn system_id(&self) -> Option<Arc<str>> {
        None
    }
    pub fn installation_id(&self) -> Option<Arc<str>> {
        None
    }
    pub fn is_staff(&self) -> Option<bool> {
        None
    }

    pub fn log_edit_event(&self, _environment: &'static str, _is_via_ssh: bool) {}

    pub fn report_discovered_project_type_events(
        &self,
        _worktree_id: WorktreeId,
        _updated_entries_set: &UpdatedEntriesSet,
    ) {
    }

    pub fn report_remote_event(
        &self,
        _event_json: &str,
        _connection_type: &str,
        _os_name: String,
        _os_version: Option<String>,
        _architecture: String,
    ) -> Result<()> {
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn queued_events(&self) -> Vec<telemetry_events::FlexibleEvent> {
        Vec::new()
    }

    pub fn flush_events(&self) -> Task<()> {
        Task::ready(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clock::FakeSystemClock;
    use gpui::TestAppContext;
    use http_client::FakeHttpClient;
    use settings::{SettingsStore, TelemetrySettingsContent};

    #[gpui::test]
    async fn legacy_settings_cannot_enable_telemetry(cx: &mut TestAppContext) {
        let http = FakeHttpClient::create(|request| async move {
            panic!("telemetry must not send a request: {}", request.uri());
        });
        let telemetry = cx.update(|cx| {
            let mut settings = SettingsStore::test(cx);
            settings.update_user_settings(cx, |settings| {
                settings.telemetry = Some(TelemetrySettingsContent {
                    diagnostics: Some(true),
                    metrics: Some(true),
                    anthropic_retention: Some(true),
                });
            });
            cx.set_global(settings);
            let telemetry = Telemetry::new(Arc::new(FakeSystemClock::new()), http, cx);
            telemetry.start(
                Some("old-system-id".into()),
                Some("old-installation-id".into()),
                "session".into(),
                cx,
            );
            telemetry
        });
        ::telemetry::event!("App Opened");
        telemetry.log_edit_event("editor", false);
        assert!(
            telemetry
                .report_remote_event("{}", "ssh", "Linux".into(), None, "x86_64".into())
                .is_ok()
        );
        telemetry.flush_events().await;
        cx.run_until_parked();
        assert!(!telemetry.metrics_enabled());
        assert!(!telemetry.diagnostics_enabled());
        assert!(telemetry.system_id().is_none());
        assert!(telemetry.installation_id().is_none());
        assert!(telemetry.metrics_id().is_none());
        assert!(telemetry.queued_events().is_empty());
    }
}
