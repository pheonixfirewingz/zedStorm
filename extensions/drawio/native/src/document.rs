use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const EMPTY_DIAGRAM: &str = "<mxfile><diagram name=\"Page-1\"><mxGraphModel><root><mxCell id=\"0\"/><mxCell id=\"1\" parent=\"0\"/></root></mxGraphModel></diagram></mxfile>";

pub struct Document {
    path: PathBuf,
    saved: Option<String>,
}

impl Document {
    pub fn open(path: &Path) -> Result<Self> {
        if path.extension().and_then(|value| value.to_str()) != Some("drawio") {
            bail!("Only .drawio XML files are supported");
        }
        let path = if path.exists() {
            path.canonicalize()?
        } else {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            parent
                .canonicalize()?
                .join(path.file_name().context("Missing filename")?)
        };
        let saved = read_optional(&path)?;
        Ok(Self { path, saved })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn xml(&self) -> &str {
        self.saved
            .as_deref()
            .filter(|xml| !xml.trim().is_empty())
            .unwrap_or(EMPTY_DIAGRAM)
    }

    pub fn save(&mut self, xml: &str) -> Result<()> {
        if xml.trim().is_empty() {
            bail!("The editor returned an empty diagram");
        }
        // Check the original contents so a second editor cannot silently overwrite external edits.
        if read_optional(&self.path)? != self.saved {
            bail!(
                "The file changed outside this window. Save your diagram elsewhere or reopen the file before saving."
            );
        }
        let parent = self.path.parent().context("Missing parent directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        if self.saved.is_some() {
            temporary
                .as_file()
                .set_permissions(fs::metadata(&self.path)?.permissions())?;
        }
        temporary.write_all(xml.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.path)
            .context("Could not replace diagram file")?;
        self.saved = Some(xml.to_string());
        Ok(())
    }
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Could not read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_updates_diagrams() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("diagram with spaces.drawio");
        let mut document = Document::open(&path)?;
        assert_eq!(document.xml(), EMPTY_DIAGRAM);
        assert!(!path.exists());
        document.save(EMPTY_DIAGRAM)?;
        document.save("<mxfile/>")?;
        assert_eq!(fs::read_to_string(path)?, "<mxfile/>");
        Ok(())
    }

    #[test]
    fn refuses_external_changes_and_deletions() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("diagram.drawio");
        fs::write(&path, EMPTY_DIAGRAM)?;
        let mut document = Document::open(&path)?;
        fs::write(&path, "external change")?;
        assert!(document.save("<mxfile/>").is_err());
        assert_eq!(fs::read_to_string(&path)?, "external change");
        fs::remove_file(&path)?;
        assert!(document.save("<mxfile/>").is_err());
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn refuses_empty_saves_and_other_formats() -> Result<()> {
        let directory = tempfile::tempdir()?;
        assert!(Document::open(&directory.path().join("diagram.drawio.png")).is_err());
        let path = directory.path().join("diagram.drawio");
        let mut document = Document::open(&path)?;
        assert!(document.save("  ").is_err());
        assert!(!path.exists());
        Ok(())
    }
}
