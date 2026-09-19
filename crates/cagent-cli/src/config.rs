use crossterm::style::Stylize as _;
use std::io::IsTerminal as _;
use std::path::PathBuf;

use super::ConfigCommand;

pub(super) fn run(
    config_file: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    command: ConfigCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths = cagent_agent::config::AppPaths::resolve(cagent_agent::config::PathOverrides {
        config_file,
        data_dir,
    })?;
    let mutating = matches!(command, ConfigCommand::Set { .. } | ConfigCommand::Edit);
    if mutating {
        paths.create_directories()?;
    }
    let config = cagent_agent::config::ConfigStore::open(&paths.config_file)?;
    match command {
        ConfigCommand::View { safe } => {
            let source = config.source()?;
            print_source(if safe {
                redact_secrets(&source)
            } else {
                source
            });
        }
        ConfigCommand::Get { key } => {
            let source = config.source()?;
            if cagent_agent::config::config_selection_is_secret(&source, &key) {
                return Err(format!("config key cannot be displayed safely: {key}").into());
            }
            let value = config
                .get_value(&key)?
                .ok_or_else(|| format!("config key is not set: {key}"))?;
            println!("{value}");
        }
        ConfigCommand::Set { key, value } => {
            config.set_value(&key, &value)?;
            println!("Set {key} to {value}");
        }
        ConfigCommand::Edit => {
            if !paths.config_file.exists() {
                std::fs::write(&paths.config_file, "version = 1\n")?;
            }
            let editor = std::env::var_os("VISUAL")
                .or_else(|| std::env::var_os("EDITOR"))
                .ok_or("set $VISUAL or $EDITOR before using config edit")?;
            let editor = editor.to_string_lossy().into_owned();
            let mut parts = editor.split_whitespace();
            let executable = parts
                .next()
                .ok_or("$VISUAL or $EDITOR must name an executable")?;
            let status = std::process::Command::new(executable)
                .args(parts)
                .arg(&paths.config_file)
                .status()?;
            if !status.success() {
                return Err(format!("editor exited with {status}").into());
            }
            config.reload()?;
        }
        ConfigCommand::List => {
            for key in config.keys()? {
                println!("{key}");
            }
        }
    }
    Ok(())
}

fn print_source(source: String) {
    let use_color = std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var_os("TERM").as_deref() != Some(std::ffi::OsStr::new("dumb"));
    for token in cagent_agent::presentation::highlight_code("toml", &source) {
        if !use_color {
            print!("{}", token.text);
            continue;
        }
        let color = match token.kind {
            cagent_agent::presentation::CodeTokenKind::Comment => crossterm::style::Color::DarkGrey,
            cagent_agent::presentation::CodeTokenKind::String => crossterm::style::Color::Green,
            cagent_agent::presentation::CodeTokenKind::Number => crossterm::style::Color::Yellow,
            cagent_agent::presentation::CodeTokenKind::Keyword
            | cagent_agent::presentation::CodeTokenKind::Attribute => crossterm::style::Color::Blue,
            cagent_agent::presentation::CodeTokenKind::Constant => crossterm::style::Color::Cyan,
            _ => crossterm::style::Color::Reset,
        };
        print!("{}", token.text.with(color));
    }
}

fn redact_secrets(source: &str) -> String {
    let Ok(mut document) = source.parse::<toml_edit::DocumentMut>() else {
        return "# Configuration could not be safely displayed.\n".into();
    };
    redact_table(document.as_table_mut());
    document.to_string()
}

fn redact_table(table: &mut toml_edit::Table) {
    for (key, item) in table.iter_mut() {
        if cagent_agent::config::config_key_is_secret(key.get()) {
            if item.is_value() {
                *item = toml_edit::value("<redacted>");
            } else if let Some(table) = item.as_table_mut() {
                redact_entire_table(table);
            }
        } else if let Some(table) = item.as_table_mut() {
            redact_table(table);
        } else if let Some(array) = item.as_array_of_tables_mut() {
            for table in array.iter_mut() {
                redact_table(table);
            }
        }
    }
}

fn redact_entire_table(table: &mut toml_edit::Table) {
    for (_, item) in table.iter_mut() {
        if item.is_value() {
            *item = toml_edit::value("<redacted>");
        } else if let Some(table) = item.as_table_mut() {
            redact_entire_table(table);
        }
    }
}
