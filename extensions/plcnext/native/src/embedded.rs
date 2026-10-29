use anyhow::{bail, Result};
use std::path::PathBuf;

pub fn run(_path: PathBuf) -> Result<()> {
    bail!("Embedded tab renderer on this platform requires a native windowing environment.");
}
