mod approval;
mod chord;
mod dashboard;
mod desktop;
mod graphql;
mod idle;
mod input;
mod keymap;
mod launch;
mod mcp;
mod runtime;

use clap::builder::{OsStringValueParser, TypedValueParser};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(version, about, propagate_version = true)]
struct Cli {
    /// Format command results
    #[arg(long, global = true, value_enum, default_value = "json")]
    output: Output,
    /// Write command diagnostics to standard error
    #[arg(long, global = true)]
    debug: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Output {
    Json,
    Text,
    Table,
}

#[derive(Subcommand)]
enum Command {
    /// The dashboard API schema
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    /// The MCP server
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// The browser dashboard that shows the desktops and pauses their agents
    Dashboard {
        #[command(subcommand)]
        command: DashboardCommand,
    },
    /// The running desktops of this user
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
}

#[derive(Subcommand)]
enum DesktopCommand {
    /// List the running desktops
    ListDesktops,
    /// Send a note to the agent of a desktop; it arrives once, appended to the
    /// result of the agent's next MCP tool call
    CreateNote {
        /// The desktop's Id (as list-desktops and the dashboard show it)
        #[arg(long, value_parser = desktop_id)]
        desktop_id: String,
        /// The note's text, or file://<path> to read it from a file
        /// (file:///dev/stdin reads standard input)
        #[arg(long, value_parser = note_text)]
        text: String,
    },
}

fn desktop_id(value: &str) -> Result<String, String> {
    let value = parameter_text(value)?;
    let value = value.trim();
    if runtime::is_id(value) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "{value:?} is not a desktop Id (6 letters or digits)"
        ))
    }
}

fn parameter_text(value: &str) -> Result<String, String> {
    match value.strip_prefix("file://") {
        Some(path) => {
            std::fs::read_to_string(path).map_err(|error| format!("cannot read {path}: {error}"))
        }
        None => Ok(value.to_string()),
    }
}

fn note_text(value: &str) -> Result<String, String> {
    runtime::check_note(&parameter_text(value)?).map(str::to_string)
}

fn address(value: &str) -> Result<std::net::SocketAddr, String> {
    let address: std::net::SocketAddr = parameter_text(value)?
        .trim()
        .parse()
        .map_err(|error: std::net::AddrParseError| error.to_string())?;
    if !address.ip().is_loopback() {
        return Err(
            "the dashboard needs a loopback address; use an HTTPS reverse proxy for remote access"
                .into(),
        );
    }
    Ok(address)
}

fn novnc_directory(value: std::ffi::OsString) -> Result<std::path::PathBuf, String> {
    match value.to_str() {
        Some(value) if value.starts_with("file://") => Ok(parameter_text(value)?.trim_end().into()),
        _ => Ok(value.into()),
    }
}

fn origin(value: &str) -> Result<String, String> {
    dashboard::parse_origin(parameter_text(value)?.trim())
}

fn login(value: &str) -> Result<String, String> {
    let value = parameter_text(value)?;
    let login = value.trim();
    if login.is_empty() {
        return Err("expected a Tailscale login".into());
    }
    Ok(login.to_owned())
}

#[derive(Subcommand)]
enum DashboardCommand {
    /// Serve the dashboard over HTTP
    #[command(alias = "serve")]
    StartDashboard {
        /// Loopback address to listen on
        #[arg(long, default_value = "127.0.0.1:8730", value_parser = address)]
        address: std::net::SocketAddr,
        /// noVNC's directory (the one that holds core/rfb.js)
        #[arg(long, env = "SELLAITE_NOVNC_DIR", value_parser = OsStringValueParser::new().try_map(novnc_directory))]
        novnc_dir: std::path::PathBuf,
        /// Origins a reverse proxy serves the dashboard under (for example
        /// `https://host.tailnet.ts.net:8731` from tailscale serve); their Host
        /// and Origin are accepted
        #[arg(long, num_args = 1.., value_parser = origin, requires = "allowed_logins")]
        public_origins: Vec<String>,
        /// Tailscale logins (the Tailscale-User-Login header) allowed to use the
        /// dashboard; every other request gets 403
        #[arg(long, num_args = 1.., value_parser = login)]
        allowed_logins: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP over stdio
    #[command(alias = "serve")]
    ServeMcp,
}

const EXIT_USAGE: i32 = 252;
const EXIT_TARGET: i32 = 254;
const EXIT_FAILURE: i32 = 255;

#[derive(Subcommand)]
enum SchemaCommand {
    /// Export the dashboard GraphQL schema
    GetSdl,
}

fn parse_cli() -> Result<Cli, clap::Error> {
    let mut args: Vec<_> = std::env::args_os().collect();
    if let [prefix @ .., group, subcommand, help] = args.as_slice()
        && !prefix.is_empty()
        && help == "help"
        && group
            .to_str()
            .and_then(|name| Cli::command().find_subcommand(name).cloned())
            .and_then(|group| {
                subcommand
                    .to_str()
                    .and_then(|name| group.find_subcommand(name).cloned())
            })
            .is_some()
    {
        *args.last_mut().unwrap() = "--help".into();
    }
    Cli::try_parse_from(args)
}

fn print_result(output: Output, result: Value) {
    if matches!(output, Output::Json) {
        println!("{result}");
        return;
    }
    let rows = result
        .get("Desktops")
        .and_then(Value::as_array)
        .map_or_else(|| std::slice::from_ref(&result), Vec::as_slice);
    let Some(fields) = rows.first().and_then(Value::as_object) else {
        return;
    };
    let render = |value: &Value| {
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned)
    };
    if matches!(output, Output::Text) {
        for row in rows {
            println!(
                "{}",
                fields
                    .keys()
                    .map(|key| render(&row[key]))
                    .collect::<Vec<_>>()
                    .join("\t")
            );
        }
    } else {
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                fields
                    .keys()
                    .map(|key| render(&row[key]).replace('\n', "\\n"))
                    .collect()
            })
            .collect();
        let widths: Vec<_> = fields
            .keys()
            .enumerate()
            .map(|(index, key)| {
                cells
                    .iter()
                    .map(|row| row[index].chars().count())
                    .fold(key.len(), usize::max)
            })
            .collect();
        let line = |cells: Vec<String>| {
            cells
                .into_iter()
                .zip(&widths)
                .map(|(cell, width)| format!("{cell:width$}"))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        println!("{}", line(fields.keys().cloned().collect()));
        for row in cells {
            println!("{}", line(row));
        }
    }
}

fn main() {
    let cli = parse_cli().unwrap_or_else(|e| {
        let code = if e.use_stderr() { EXIT_USAGE } else { 0 };
        let _ = e.print();
        std::process::exit(code);
    });
    if cli.debug {
        eprintln!("sellaite: command parsed");
    }
    match cli.command {
        Command::Schema {
            command: SchemaCommand::GetSdl,
        } => {
            print_result(
                cli.output,
                json!({"Sdl": graphql::schema().sdl().trim_end()}),
            );
        }
        Command::Mcp {
            command: McpCommand::ServeMcp,
        } => {
            for dir in runtime::remove_stale() {
                eprintln!(
                    "sellaite: removed the stale runtime directory {}",
                    dir.display()
                );
            }
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            if let Err(e) = mcp::Server::default().serve(stdin, stdout) {
                eprintln!("sellaite: {e}");
                std::process::exit(EXIT_FAILURE);
            }
        }
        Command::Dashboard {
            command:
                DashboardCommand::StartDashboard {
                    address,
                    novnc_dir,
                    public_origins,
                    allowed_logins,
                },
        } => {
            if let Err(e) = dashboard::serve(address, &novnc_dir, &public_origins, allowed_logins) {
                eprintln!("sellaite: {e}");
                std::process::exit(EXIT_FAILURE);
            }
        }
        Command::Desktop {
            command: DesktopCommand::ListDesktops,
        } => print_result(cli.output, json!({"Desktops": graphql::Desktop::list()})),
        Command::Desktop {
            command: DesktopCommand::CreateNote { desktop_id, text },
        } => {
            let Some(dir) = runtime::live_dir(&desktop_id) else {
                eprintln!("sellaite: no desktop {desktop_id} is running");
                std::process::exit(EXIT_TARGET);
            };
            match runtime::add_note(&dir, &text) {
                Ok(_) => print_result(cli.output, json!(graphql::Desktop::read(&desktop_id, &dir))),
                Err(e) => {
                    eprintln!("sellaite: {e}");
                    std::process::exit(EXIT_FAILURE);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
