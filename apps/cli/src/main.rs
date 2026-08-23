use howler_app::NoteFolder;
use std::env;
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("versions") => println!("editor={} application={} editor_abi={} application_abi={} index_schema={} state_schema={}", howler_editor::EDITOR_VERSION, howler_app::APPLICATION_VERSION, howler_editor_ffi::ABI_VERSION, howler_application_ffi::ABI_VERSION, howler_app::INDEX_SCHEMA_VERSION, howler_app::STATE_SCHEMA_VERSION),
        Some("validate") | Some("rebuild") | Some("rescan") | Some("recoveries") | Some("bundle") => {
            let command = args[0].as_str();
            let folder_path = args.get(1).ok_or("missing note folder")?;
            let folder = NoteFolder::open(folder_path, state_path(&args, 2)?, false)?;
            let output = match command {
                "validate" => serde_json::to_value(folder.diagnostics()?)?,
                "rebuild" => serde_json::to_value(folder.rebuild_index()?)?,
                "rescan" => serde_json::to_value(folder.rescan()?)?,
                "recoveries" => serde_json::to_value(folder.recoveries()?)?,
                "bundle" => serde_json::to_value(folder.diagnostic_bundle()?)?,
                _ => unreachable!(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Some("search") => {
            let folder_path = args.get(1).ok_or("missing note folder")?;
            let query = args.get(2).ok_or("missing search query")?;
            let folder = NoteFolder::open(folder_path, state_path(&args, 3)?, false)?;
            println!("{}", serde_json::to_string_pretty(&folder.search(query, 20)?)?);
        }
        _ => eprintln!("Usage:\n  howler versions\n  howler validate <folder> [--state <path>]\n  howler rebuild <folder> [--state <path>]\n  howler rescan <folder> [--state <path>]\n  howler recoveries <folder> [--state <path>]\n  howler bundle <folder> [--state <path>]\n  howler search <folder> <query> [--state <path>]"),
    }
    Ok(())
}

fn state_path(args: &[String], offset: usize) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if args.get(offset).map(String::as_str) == Some("--state") {
        return Ok(PathBuf::from(
            args.get(offset + 1).ok_or("missing --state value")?,
        ));
    }
    let home = env::var_os("HOME").ok_or("HOME is unavailable; pass --state")?;
    Ok(Path::new(&home).join(".local/share/howler"))
}
