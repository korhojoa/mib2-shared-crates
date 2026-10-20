// SPDX-License-Identifier: MIT OR Apache-2.0
use extender_settings::{base_path, store, wall_ms};
use std::path::Path;

fn run() -> Result<String, String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let base = if args.first().map(String::as_str) == Some("--base") && args.len() >= 2 {
        let path = args.remove(1);
        args.remove(0);
        std::path::PathBuf::from(path)
    } else {
        base_path()?
    };
    if args.len() < 2 {
        return Err(
            "usage: android-auto-settings [--base FILE] CARD_DIR snapshot|commit [REV key=value ...]".into(),
        );
    }
    let dir = Path::new(&args[0]);
    extender_settings::location::publish_from_hmi(dir)?;
    // A save is only useful where gal's reader looks; both sides resolve the
    // directory with the same rule.
    #[cfg(target_os = "nto")]
    {
        let reader = extender_settings::settings_dir(&base);
        if dir != reader {
            return Err(format!(
                "settings directory {} is not the one gal reads ({}); set {}={} in {}",
                dir.display(),
                reader.display(),
                extender_settings::SETTINGS_DIR_KEY,
                dir.display(),
                base.display()
            ));
        }
    }
    let snapshot = match args[1].as_str() {
        "snapshot" if args.len() == 2 => store::load(&base, dir)?,
        "commit" if args.len() >= 4 => {
            let edits: Result<Vec<_>, _> = args[3..]
                .iter()
                .map(|s| {
                    s.split_once('=')
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .ok_or("expected key=value")
                })
                .collect();
            let edits = edits?;
            extender_settings::card_mount::with_writable(dir, || {
                store::commit(&base, dir, &args[2], &edits)
            })?
        }
        _ => return Err("invalid settings operation".into()),
    };
    Ok(extender_settings::snapshot_text(&snapshot, dir))
}

fn main() {
    let start = std::time::Instant::now();
    match run() {
        Ok(text) => extender_settings::emit(&text),
        Err(error) => {
            let message = error.replace(['\n', '\r', '\t'], " ");
            eprintln!(
                "mono_ms={} wall_ms={} AndroidAutoSettings: {message}",
                start.elapsed().as_millis(),
                wall_ms()
            );
            extender_settings::emit(&format!("error\t{message}\nend\t1\n"));
            std::process::exit(1);
        }
    }
}
