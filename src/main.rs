mod chord;
mod dashboard;
mod desktop;
mod idle;
mod input;
mod keymap;
mod launch;
mod mcp;
mod runtime;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    if runtime::is_id(value) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "{value:?} is not a desktop Id (6 letters or digits)"
        ))
    }
}

fn note_text(value: &str) -> Result<String, String> {
    let text = match value.strip_prefix("file://") {
        Some(path) => {
            std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?
        }
        None => value.to_string(),
    };
    runtime::check_note(&text).map(str::to_string)
}

#[derive(Subcommand)]
enum DashboardCommand {
    /// Serve the dashboard over HTTP
    Serve {
        /// Address to listen on
        #[arg(long, default_value = "127.0.0.1:8730")]
        address: std::net::SocketAddr,
        /// noVNC's directory (the one that holds core/rfb.js)
        #[arg(long, env = "SELLAITE_NOVNC_DIR")]
        novnc_dir: std::path::PathBuf,
        /// Origins a reverse proxy serves the dashboard under (for example
        /// `https://host.tailnet.ts.net:8731` from tailscale serve); their Host
        /// and Origin are accepted
        #[arg(long, num_args = 1.., value_parser = dashboard::parse_origin, requires = "allowed_logins")]
        public_origins: Vec<String>,
        /// Tailscale logins (the Tailscale-User-Login header) allowed to use the
        /// dashboard; every other request gets 403
        #[arg(long, num_args = 1..)]
        allowed_logins: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP over stdio
    Serve,
}

const EXIT_USAGE: i32 = 252;
const EXIT_TARGET: i32 = 254;
const EXIT_FAILURE: i32 = 255;

fn main() {
    let cli = Cli::try_parse().unwrap_or_else(|e| {
        let code = if e.use_stderr() { EXIT_USAGE } else { 0 };
        let _ = e.print();
        std::process::exit(code);
    });
    match cli.command {
        Command::Mcp {
            command: McpCommand::Serve,
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
                DashboardCommand::Serve {
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
        } => println!("{}", dashboard::desktops()),
        Command::Desktop {
            command: DesktopCommand::CreateNote { desktop_id, text },
        } => {
            let Some(dir) = runtime::live_dir(&desktop_id) else {
                eprintln!("sellaite: no desktop {desktop_id} is running");
                std::process::exit(EXIT_TARGET);
            };
            match runtime::add_note(&dir, &text) {
                Ok(_) => println!("{}", dashboard::desktop_json(&desktop_id, &dir)),
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
