use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn run(workspace: &Path, config: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cagent"))
        .current_dir(workspace)
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn success(workspace: &Path, config: &Path, data: &Path, args: &[&str]) -> Output {
    let output = run(workspace, config, data, args);
    assert!(
        output.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn success_with_stdin(
    workspace: &Path,
    config: &Path,
    data: &Path,
    args: &[&str],
    input: &str,
) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cagent"))
        .current_dir(workspace)
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
#[allow(clippy::too_many_lines)]
fn every_mcp_cli_command_preserves_comments_confirms_and_enforces_trust() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let data = temp.path().join("data");
    std::fs::create_dir(&workspace).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(&config, "# preserved\nversion = 1\n").unwrap();

    success(
        &workspace,
        &config,
        &data,
        &["mcp", "add", "docs", "--", "docs-server", "serve"],
    );
    let human = success(&workspace, &config, &data, &["mcp", "list"]);
    assert!(String::from_utf8_lossy(&human.stdout).contains("docs\tGlobal\tenabled"));
    let get = success(
        &workspace,
        &config,
        &data,
        &["mcp", "get", "docs", "--json"],
    );
    let get: serde_json::Value = serde_json::from_slice(&get.stdout).unwrap();
    assert_eq!(get["name"], "docs");
    assert_eq!(get["definition"]["command"], "docs-server");

    success(&workspace, &config, &data, &["mcp", "disable", "docs"]);
    let disabled = success(
        &workspace,
        &config,
        &data,
        &["mcp", "get", "docs", "--json"],
    );
    assert!(!serde_json::from_slice::<serde_json::Value>(&disabled.stdout).unwrap()
        ["definition"]["enabled"]
        .as_bool()
        .unwrap());
    success(&workspace, &config, &data, &["mcp", "enable", "docs"]);

    success(
        &workspace,
        &config,
        &data,
        &[
            "mcp",
            "add",
            "review-docs",
            "--scope",
            "global",
            "--",
            "review-server",
        ],
    );
    let all = success(
        &workspace,
        &config,
        &data,
        &["mcp", "list", "--all", "--json"],
    );
    let all = String::from_utf8_lossy(&all.stdout);
    assert!(all.contains("review-docs"));
    assert!(all.contains("review-server"));

    let import = temp.path().join("servers.json");
    std::fs::write(
        &import,
        r#"{"servers":{"remote":{"type":"http","url":"https://example.test/mcp"}}}"#,
    )
    .unwrap();
    success(
        &workspace,
        &config,
        &data,
        &["mcp", "add", "--json", import.to_str().unwrap()],
    );
    success_with_stdin(
        &workspace,
        &config,
        &data,
        &["mcp", "add", "stdin-server", "--json"],
        r#"{"command":"stdin-command"}"#,
    );

    let replacement = run(
        &workspace,
        &config,
        &data,
        &["mcp", "add", "docs", "--", "replacement"],
    );
    assert!(!replacement.status.success());
    assert!(String::from_utf8_lossy(&replacement.stderr).contains("confirmation is required"));
    success(
        &workspace,
        &config,
        &data,
        &["mcp", "add", "docs", "--yes", "--", "replacement"],
    );

    let removal = run(&workspace, &config, &data, &["mcp", "remove", "remote"]);
    assert!(!removal.status.success());
    success(
        &workspace,
        &config,
        &data,
        &["mcp", "remove", "remote", "--yes"],
    );

    let project = run(
        &workspace,
        &config,
        &data,
        &[
            "mcp",
            "add",
            "project-only",
            "--scope",
            "project",
            "--",
            "project-server",
        ],
    );
    assert!(!project.status.success());
    assert!(String::from_utf8_lossy(&project.stderr).contains("workspace trust"));
    cagent_agent::permissions::PermissionFile::new(
        config.with_file_name("permissions.toml"),
        &workspace,
    )
    .unwrap()
    .set_trusted(true)
    .unwrap();
    success(
        &workspace,
        &config,
        &data,
        &[
            "mcp",
            "add",
            "project-only",
            "--scope",
            "project",
            "--",
            "project-server",
        ],
    );
    assert!(
        std::fs::read_to_string(workspace.join(".cagent/config.toml"))
            .unwrap()
            .contains("project-server")
    );
    assert!(
        std::fs::read_to_string(&config)
            .unwrap()
            .starts_with("# preserved")
    );
}
