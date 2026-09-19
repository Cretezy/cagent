use std::collections::BTreeMap;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::PathBuf;

use cagent_agent::config::{AppPaths, ConfigStore, PathOverrides};
use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Subcommand)]
pub(super) enum McpCommand {
    Add {
        name: Option<String>,
        #[arg(long, value_enum, default_value = "global")]
        scope: CliMcpScope,
        #[arg(long, conflicts_with = "json")]
        url: Option<String>,
        #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "-")]
        json: Option<PathBuf>,
        #[arg(long)]
        yes: bool,
        #[arg(last = true)]
        command: Vec<String>,
    },
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    Get {
        name: String,
        #[arg(long)]
        json: bool,
    },
    Enable(McpTarget),
    Disable(McpTarget),
    Remove {
        #[command(flatten)]
        target: McpTarget,
        #[arg(long)]
        yes: bool,
    },
    Catalog {
        #[arg(long)]
        json: bool,
    },
    Install {
        source: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum, default_value = "global")]
        scope: CliMcpScope,
        #[arg(long)]
        yes: bool,
    },
    Detach(McpTarget),
    Secret {
        #[command(subcommand)]
        command: McpSecretCommand,
    },
    OAuth {
        #[command(subcommand)]
        command: McpOAuthCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(super) enum McpSecretCommand {
    Set { reference: String },
    Remove { reference: String },
    Status { reference: String },
}

#[derive(Debug, Subcommand)]
pub(super) enum McpOAuthCommand {
    Connect(McpTarget),
    Disconnect(McpTarget),
    Status(McpTarget),
}

#[derive(Debug, Args)]
pub(super) struct McpTarget {
    pub(super) name: String,
    #[arg(long, value_enum, default_value = "global")]
    pub(super) scope: CliMcpScope,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum CliMcpScope {
    Global,
    Project,
}

pub(super) fn run(
    config_file: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    command: McpCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = std::env::current_dir()?.canonicalize()?;
    let paths = AppPaths::resolve(PathOverrides {
        config_file,
        data_dir,
    })?;
    paths.create_directories()?;
    let permissions =
        cagent_agent::permissions::PermissionFile::new(paths.permissions_file.clone(), &workspace)?;
    let trusted = permissions.is_trusted()?;
    let config = ConfigStore::open(&paths.config_file)?;
    let default_agent = config.snapshot().default_agent().to_owned();
    let service =
        cagent_agent::mcp::McpConfigService::from_config_store(config, &workspace, trusted)?;
    let secrets = cagent_agent::mcp::McpSecretStore::new(&paths.data_dir);
    match command {
        McpCommand::Add {
            name,
            scope,
            url,
            json,
            yes,
            command,
        } => {
            let location = location(scope);
            if let Some(path) = json {
                let source = read_json_source(&path)?;
                let preview = service.preview_import_json(location, &source, name.as_deref())?;
                let confirmed = confirm(
                    preview.requires_confirmation,
                    yes,
                    "Replace existing MCP definitions?",
                )?;
                let servers = service.apply_import(&default_agent, preview, confirmed)?;
                print_servers(&servers, false)?;
            } else {
                let name =
                    name.ok_or("mcp add requires NAME unless imported JSON contains names")?;
                let transport = if let Some(url) = url {
                    if !command.is_empty() {
                        return Err("--url cannot be combined with a stdio command".into());
                    }
                    cagent_agent::mcp::McpTransportConfig::StreamableHttp {
                        url,
                        headers: BTreeMap::default(),
                        allow_insecure: false,
                    }
                } else {
                    let (executable, args) = command
                        .split_first()
                        .ok_or("stdio mcp add requires an executable after --")?;
                    cagent_agent::mcp::McpTransportConfig::Stdio {
                        command: executable.clone(),
                        args: args.to_vec(),
                        cwd: None,
                        env: BTreeMap::default(),
                        env_remove: Vec::new(),
                        inherit_env: true,
                    }
                };
                let definition = cagent_agent::mcp::McpServerDefinition {
                    transport,
                    enabled: true,
                    agents: Vec::new(),
                    eager: false,
                    startup_timeout_seconds: 10,
                    request_timeout_seconds: 60,
                    read_only_tools: Vec::new(),
                    ..cagent_agent::mcp::McpServerDefinition::default()
                };
                let preview = service.preview_mutations(
                    &default_agent,
                    vec![cagent_agent::mcp::McpMutation::Put {
                        location,
                        name,
                        definition,
                    }],
                )?;
                let confirmed = confirm(
                    preview.requires_confirmation,
                    yes,
                    "Replace the existing MCP definition?",
                )?;
                let servers = service.apply_mutations(&default_agent, preview, confirmed)?;
                print_servers(&servers, false)?;
            }
        }
        McpCommand::List { all, json } => print_servers(
            &if all {
                service.list_all_effective(&default_agent)?
            } else {
                service.list_effective(&default_agent)?
            },
            json,
        )?,
        McpCommand::Get { name, json } => {
            let server = service
                .get_effective(&default_agent, &name)?
                .ok_or_else(|| format!("unknown MCP server: {name}"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&server)?);
            } else {
                println!(
                    "{}\t{:?}\t{}\t{:?}",
                    server.name,
                    server.location.scope,
                    if server.definition.enabled && server.allowed_for_agent {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    server.status
                );
            }
        }
        McpCommand::Enable(target) => print_servers(
            &service.set_enabled(&default_agent, location(target.scope), &target.name, true)?,
            false,
        )?,
        McpCommand::Disable(target) => print_servers(
            &service.set_enabled(&default_agent, location(target.scope), &target.name, false)?,
            false,
        )?,
        McpCommand::Remove { target, yes } => {
            if !confirm(true, yes, "Remove this MCP definition?")? {
                return Ok(());
            }
            let preview = service.preview_mutations(
                &default_agent,
                vec![cagent_agent::mcp::McpMutation::Remove {
                    location: location(target.scope),
                    name: target.name,
                }],
            )?;
            print_servers(
                &service.apply_mutations(&default_agent, preview, true)?,
                false,
            )?;
        }
        McpCommand::Catalog { json } => {
            let packages = cagent_agent::mcp::builtin_packages();
            if json {
                println!("{}", serde_json::to_string_pretty(&packages)?);
            } else {
                for package in packages {
                    println!(
                        "{}\t{}\t{}",
                        package.id, package.version, package.description
                    );
                }
            }
        }
        McpCommand::Install {
            source,
            name,
            scope,
            yes,
        } => {
            let install_location = location(scope);
            let (package, digest) = if source.starts_with("https://") {
                let runtime = tokio::runtime::Runtime::new()?;
                let (package, digest) = runtime.block_on(cagent_agent::mcp::fetch_url_package(
                    &service.path_for_location(&install_location)?,
                    &source,
                ))?;
                (package, Some(digest))
            } else {
                let id = source.strip_prefix("builtin:").unwrap_or(&source);
                let package = cagent_agent::mcp::builtin_packages()
                    .into_iter()
                    .find(|package| package.id == id)
                    .ok_or_else(|| format!("unknown built-in MCP package: {id}"))?;
                (package, None)
            };
            let source = if source.starts_with("https://") {
                source
            } else {
                format!("builtin:{}", package.id)
            };
            let server_name = name.unwrap_or_else(|| package.id.clone());
            let definition = package.resolve(
                &source,
                &server_name,
                digest,
                package
                    .parameters
                    .iter()
                    .filter_map(|(id, parameter)| {
                        parameter.default.clone().map(|value| (id.clone(), value))
                    })
                    .collect(),
            )?;
            let preview = service.preview_mutations(
                &default_agent,
                vec![cagent_agent::mcp::McpMutation::Put {
                    location: install_location,
                    name: server_name,
                    definition,
                }],
            )?;
            let confirmed = confirm(preview.requires_confirmation, yes, "Replace existing MCP?")?;
            print_servers(
                &service.apply_mutations(&default_agent, preview, confirmed)?,
                false,
            )?;
        }
        McpCommand::Detach(target) => {
            let mut definition = service
                .list_all()?
                .into_iter()
                .find(|(server_location, name, _)| {
                    server_location == &location(target.scope) && name == &target.name
                })
                .map(|(_, _, definition)| definition)
                .ok_or("unknown MCP definition")?;
            definition.package = None;
            definition.package_digest = None;
            definition.parameters.clear();
            let preview = service.preview_mutations(
                &default_agent,
                vec![cagent_agent::mcp::McpMutation::Put {
                    location: location(target.scope),
                    name: target.name,
                    definition,
                }],
            )?;
            print_servers(
                &service.apply_mutations(&default_agent, preview, true)?,
                false,
            )?;
        }
        McpCommand::Secret { command } => match command {
            McpSecretCommand::Set { reference } => {
                let value = read_secret()?;
                secrets.set(&reference, &value)?;
                println!("configured\t{reference}");
            }
            McpSecretCommand::Remove { reference } => {
                secrets.delete(&reference)?;
                println!("removed\t{reference}");
            }
            McpSecretCommand::Status { reference } => {
                println!("{:?}\t{reference}", secrets.status(&reference)?);
            }
        },
        McpCommand::OAuth { command } => {
            let target = match &command {
                McpOAuthCommand::Connect(target)
                | McpOAuthCommand::Disconnect(target)
                | McpOAuthCommand::Status(target) => target,
            };
            let target_name = target.name.clone();
            let target_scope = target.scope;
            let definition = service
                .list_all()?
                .into_iter()
                .find(|(server_location, name, _)| {
                    server_location == &location(target_scope) && name == &target_name
                })
                .map(|(_, _, definition)| definition)
                .ok_or("unknown MCP definition")?;
            let cagent_agent::mcp::McpTransportConfig::StreamableHttp { url, .. } =
                &definition.transport
            else {
                return Err("MCP OAuth is only available for HTTP servers".into());
            };
            let oauth = definition.oauth.clone().unwrap_or_default();
            match command {
                McpOAuthCommand::Connect(_) => {
                    let runtime = tokio::runtime::Runtime::new()?;
                    let flow = runtime.block_on(cagent_agent::mcp::begin_oauth(
                        url,
                        &oauth,
                        secrets.clone(),
                    ))?;
                    match &flow.prompt {
                        cagent_agent::mcp::McpOAuthPrompt::Browser {
                            authorization_url, ..
                        } => {
                            println!("Open this URL to connect {target_name}:\n{authorization_url}")
                        }
                        cagent_agent::mcp::McpOAuthPrompt::Device {
                            verification_url,
                            user_code,
                        } => println!(
                            "Open {verification_url} and enter code {user_code} to connect {target_name}"
                        ),
                    }
                    runtime.block_on(flow.complete())?;
                    println!("connected\t{target_name}");
                }
                McpOAuthCommand::Disconnect(_) => {
                    cagent_agent::mcp::clear_oauth(url, &secrets)?;
                    println!("disconnected\t{target_name}");
                }
                McpOAuthCommand::Status(_) => {
                    println!(
                        "{:?}\t{}",
                        cagent_agent::mcp::oauth_status(url, &secrets)?,
                        target_name
                    );
                }
            }
        }
    }
    Ok(())
}

fn read_secret() -> Result<String, Box<dyn std::error::Error>> {
    if std::io::stdin().is_terminal() {
        eprint!("Secret: ");
        std::io::stderr().flush()?;
    }
    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() {
        return Err("secret must not be empty".into());
    }
    Ok(value)
}

fn location(scope: CliMcpScope) -> cagent_agent::mcp::McpLocation {
    match scope {
        CliMcpScope::Global => cagent_agent::mcp::McpLocation::global(),
        CliMcpScope::Project => cagent_agent::mcp::McpLocation::project(),
    }
}
fn read_json_source(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    if path == std::path::Path::new("-") {
        let mut source = String::new();
        std::io::stdin().read_to_string(&mut source)?;
        Ok(source)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}
fn confirm(required: bool, yes: bool, prompt: &str) -> Result<bool, Box<dyn std::error::Error>> {
    if !required || yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err("confirmation is required; pass --yes in non-interactive use".into());
    }
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
fn print_servers(
    servers: &[cagent_agent::mcp::McpEffectiveServer],
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(servers)?);
    } else {
        for server in servers {
            println!(
                "{}\t{:?}\t{}\t{:?}",
                server.name,
                server.location.scope,
                if server.definition.enabled && server.allowed_for_agent {
                    "enabled"
                } else {
                    "disabled"
                },
                server.status
            );
        }
    }
    Ok(())
}
