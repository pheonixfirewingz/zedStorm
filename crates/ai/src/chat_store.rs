use super::{
    Backend,
    codex_protocol::{Message, Session, SessionSettings},
    mistral_api::Conversation,
};
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Serialize, Deserialize)]
pub struct SavedChat {
    pub backend: Backend,
    pub directory: PathBuf,
    pub thread_id: Option<String>,
    pub settings: SessionSettings,
    pub messages: Vec<Message>,
    pub conversation: Conversation,
}

impl SavedChat {
    pub fn capture(backend: Backend, session: &Session, conversation: Conversation) -> Self {
        Self {
            backend,
            directory: session.directory.clone(),
            thread_id: session.thread_id.clone(),
            settings: session.settings.clone(),
            messages: session.messages.clone(),
            conversation,
        }
    }

    pub fn into_session(self) -> (Backend, Session, super::mistral_api::SharedConversation) {
        let mut session = Session::new(self.directory);
        session.thread_id = self.thread_id;
        session.settings = self.settings;
        session.messages = self.messages;
        (
            self.backend,
            session,
            std::sync::Arc::new(std::sync::Mutex::new(self.conversation)),
        )
    }
}

#[derive(Serialize, Deserialize)]
pub struct SavedChats {
    pub active: SavedChat,
    pub inactive: Option<SavedChat>,
    pub history: Vec<SavedChat>,
}

pub fn storage_path(directory: &Path) -> PathBuf {
    let digest = Sha256::digest(directory.as_os_str().as_encoded_bytes());
    paths::data_dir()
        .join("chat_history")
        .join(format!("{digest:x}.json"))
}

pub fn load(path: &Path) -> Result<Option<SavedChats>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("Could not read saved chats")?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("Could not open saved chats"),
    }
}

pub fn save(path: &Path, chats: &SavedChats) -> Result<()> {
    let directory = path.parent().context("Chat history path has no parent")?;
    std::fs::create_dir_all(directory)?;
    static NEXT_WRITE: AtomicU64 = AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        NEXT_WRITE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec(chats)?)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex_protocol::MessageKind;

    #[test]
    fn chats_survive_reopening_and_keep_backends_and_resume_ids() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("history.json");
        assert!(load(&path)?.is_none());
        let mut session = Session::new(directory.path().into());
        session.thread_id = Some("codex-thread".into());
        session.busy = true;
        session.messages.push(Message {
            id: "user-1".into(),
            kind: MessageKind::User,
            text: "Keep this conversation".into(),
        });
        let chats = SavedChats {
            active: SavedChat::capture(Backend::Codex, &session, Conversation::default()),
            inactive: Some(SavedChat::capture(
                Backend::Mistral,
                &session,
                serde_json::from_value(
                    serde_json::json!({"messages":[{"role":"user","content":"Earlier prompt"}],"items":[],"next_turn":3}),
                )?,
            )),
            history: vec![SavedChat::capture(
                Backend::Codex,
                &session,
                Conversation::default(),
            )],
        };
        save(&path, &chats)?;
        let restored = load(&path)?.context("Missing saved chats")?;
        let (backend, session, _) = restored.active.into_session();
        assert!(backend == Backend::Codex);
        assert_eq!(session.thread_id.as_deref(), Some("codex-thread"));
        assert_eq!(
            session
                .messages
                .first()
                .map(|message| message.text.as_str()),
            Some("Keep this conversation")
        );
        assert!(!session.busy);
        assert!(!session.ready);
        let inactive = restored.inactive.context("Missing inactive chat")?;
        assert!(inactive.backend == Backend::Mistral);
        assert_eq!(serde_json::to_value(inactive.conversation)?["next_turn"], 3);
        assert_eq!(restored.history.len(), 1);
        save(
            &path,
            &SavedChats {
                active: SavedChat::capture(backend, &session, Conversation::default()),
                inactive: None,
                history: Vec::new(),
            },
        )?;
        assert!(
            load(&path)?
                .context("Missing replacement")?
                .history
                .is_empty()
        );
        std::fs::write(&path, b"invalid JSON")?;
        assert!(load(&path).is_err());
        Ok(())
    }
}
