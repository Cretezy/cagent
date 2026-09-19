use std::path::Path;
use std::process::Command;

fn run(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cagent"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn config_commands_honor_dir_and_manage_toml_values() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();

    let set = run(
        temporary.path(),
        &[
            "--dir",
            "workspace",
            "config",
            "set",
            "default_mode",
            "edit",
            "--config",
            "config.toml",
            "--data-dir",
            "data",
        ],
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&set.stdout).trim(),
        "Set default_mode to edit"
    );
    assert!(workspace.join("config.toml").exists());

    let get = run(
        temporary.path(),
        &[
            "--dir",
            "workspace",
            "config",
            "get",
            "default_mode",
            "--config",
            "config.toml",
            "--data-dir",
            "data",
        ],
    );
    assert!(get.status.success());
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "\"edit\"");

    let list = run(
        temporary.path(),
        &[
            "--dir",
            "workspace",
            "config",
            "list",
            "--config",
            "config.toml",
            "--data-dir",
            "data",
        ],
    );
    assert!(list.status.success());
    assert!(
        String::from_utf8_lossy(&list.stdout)
            .lines()
            .any(|key| key == "default_mode")
    );

    let view = run(
        temporary.path(),
        &[
            "--dir",
            "workspace",
            "config",
            "view",
            "--config",
            "config.toml",
            "--data-dir",
            "data",
        ],
    );
    assert!(view.status.success());
    assert!(String::from_utf8_lossy(&view.stdout).contains("default_mode"));
    assert!(!String::from_utf8_lossy(&view.stdout).contains('\x1b'));
}

#[test]
fn config_set_validates_compaction_values() {
    let temporary = tempfile::tempdir().unwrap();
    let good = run(
        temporary.path(),
        &[
            "config",
            "set",
            "compaction.threshold_percent",
            "75",
            "--config",
            "config.toml",
            "--data-dir",
            "data",
        ],
    );
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    let source = std::fs::read_to_string(temporary.path().join("config.toml")).unwrap();
    assert!(source.contains("threshold_percent = 75"));

    let bad = run(
        temporary.path(),
        &[
            "config",
            "set",
            "compaction.threshold_percent",
            "0",
            "--config",
            "config.toml",
            "--data-dir",
            "data",
        ],
    );
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("integer from 1 to 100"));
}

#[test]
fn config_view_safe_redacts_secrets_but_keeps_environment_variable_names() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::write(
        temporary.path().join("config.toml"),
        "version = 1\napi_key = 'actual-secret'\napi_key_env = 'SAFE_ENV_NAME'\napi_key_env_var = 'ALSO_SAFE_ENV_NAME'\n[headers]\nAuthorization = 'Bearer private'\n",
    )
    .unwrap();
    let output = run(
        temporary.path(),
        &[
            "config",
            "view",
            "--safe",
            "--config",
            "config.toml",
            "--data-dir",
            "data",
        ],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("actual-secret"));
    assert!(!stdout.contains("Bearer private"));
    assert!(stdout.contains("<redacted>"));
    assert!(stdout.contains("SAFE_ENV_NAME"));
    assert!(stdout.contains("ALSO_SAFE_ENV_NAME"));
}

#[test]
fn read_only_config_commands_do_not_create_directories_and_get_rejects_secrets() {
    let temporary = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "config",
            "view",
            "--safe",
            "--config",
            "missing/config.toml",
            "--data-dir",
            "data",
        ],
        vec![
            "config",
            "list",
            "--config",
            "missing/config.toml",
            "--data-dir",
            "data",
        ],
    ] {
        let output = run(temporary.path(), &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!temporary.path().join("missing").exists());
        assert!(!temporary.path().join("data").exists());
    }

    std::fs::write(
        temporary.path().join("config.toml"),
        "version = 1\napi_key = 'secret'\napi_key_env = 'OPENAI_API_KEY'\n[provider]\ntoken = 'nested-secret'\n",
    ).unwrap();
    let secret = run(
        temporary.path(),
        &["config", "get", "api_key", "--config", "config.toml"],
    );
    assert!(!secret.status.success());
    assert!(!String::from_utf8_lossy(&secret.stderr).contains("'secret'"));
    let parent = run(
        temporary.path(),
        &["config", "get", "provider", "--config", "config.toml"],
    );
    assert!(!parent.status.success());
    let env = run(
        temporary.path(),
        &["config", "get", "api_key_env", "--config", "config.toml"],
    );
    assert!(env.status.success());
    assert!(String::from_utf8_lossy(&env.stdout).contains("OPENAI_API_KEY"));
}
