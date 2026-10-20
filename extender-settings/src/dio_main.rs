// SPDX-License-Identifier: MIT OR Apache-2.0
use extender_settings::{carplay, wall_ms};
use std::path::Path;

fn run() -> Result<String, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err(
            "usage: carplay-settings CONFIG|auto snapshot|commit [REV key=value ...]".into(),
        );
    }
    let resolved = if args[0] == "auto" {
        carplay::base_path()?
    } else {
        std::path::PathBuf::from(&args[0])
    };
    let path = Path::new(&resolved);
    let snapshot = match args[1].as_str() {
        "snapshot" if args.len() == 2 => match carplay::load_present(path)? {
            Some(snapshot) => snapshot,
            None => {
                let name = carplay::absent_text(&args[0], path);
                return Ok(carplay::absent_protocol(&name));
            }
        },
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
            extender_settings::card_mount::with_writable(path, || {
                carplay::commit(path, &args[2], &edits)
            })?
        }
        _ => return Err("invalid settings operation".into()),
    };
    Ok(snapshot.protocol())
}

fn main() {
    let start = std::time::Instant::now();
    match run() {
        Ok(text) => extender_settings::emit(&text),
        Err(error) => {
            let message = error.replace(['\n', '\r', '\t'], " ");
            eprintln!(
                "mono_ms={} wall_ms={} carplay-settings: {message}",
                start.elapsed().as_millis(),
                wall_ms()
            );
            extender_settings::emit(&format!("error\t{message}\nend\t1\n"));
            std::process::exit(1);
        }
    }
}
