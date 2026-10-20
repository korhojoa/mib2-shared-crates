// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{
    carplay::{Snapshot, LIMIT},
    store::{lock_writer, read_bounded},
};
use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

pub fn load(path: &Path) -> Result<Snapshot, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("config unavailable: {e}"))?;
    if !meta.is_file() {
        return Err("config must be a regular file, not a link".into());
    }
    Snapshot::parse(read_bounded(path)?)
}

/// None when nothing exists at the path: a unit without CarPlay (dio-extender)
/// has no config, and that is a normal state, not an error. Every other
/// failure (a link, a directory, a read error) stays an error.
pub fn load_present(path: &Path) -> Result<Option<Snapshot>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        _ => load(path).map(Some),
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn prepare(path: &Path, text: &str, permissions: Permissions) -> Result<Temporary, String> {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".lsd-stage-{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    // create_new means a leftover file or a link can never be overwritten.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let guard = Temporary(temporary);
    file.set_permissions(permissions)
        .map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(guard)
}

pub fn commit(path: &Path, token: &str, edits: &[(String, String)]) -> Result<Snapshot, String> {
    let lock_path = path.with_extension("lsd-lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|m| !m.is_file()) {
        return Err("invalid writer lock file".into());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    lock_writer(&lock)?;
    let current = load(path)?;
    if current.token() != token {
        return Err("settings changed; reload before saving".into());
    }
    let next = current.changed(edits)?;
    if current.text == next {
        return Ok(current);
    }
    if next.len() > LIMIT {
        return Err("config exceeds native buffer".into());
    }
    let permissions = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
    if permissions.readonly() {
        return Err("config is read only".into());
    }
    let staged = prepare(path, &next, permissions.clone())?;
    let backup = path.with_extension("lsd-backup");
    let previous = prepare(&backup, &current.text, permissions)?;
    // A non-cooperating editor is detected before replacing either config.
    if load(path)?.text != current.text {
        return Err("config changed while saving".into());
    }
    fs::rename(&previous.0, &backup).map_err(|e| e.to_string())?;
    fs::rename(&staged.0, path).map_err(|e| e.to_string())?;
    let saved = load(path)?;
    if saved.text != next {
        return Err("config changed during verification; reload settings".into());
    }
    // Flush the renamed file too. The backup contains the complete previous file.
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(saved)
}
