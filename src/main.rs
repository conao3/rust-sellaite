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
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP over stdio
    Serve,
}

const EXIT_USAGE: i32 = 252;
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
            command: DashboardCommand::Serve { address, novnc_dir },
        } => {
            if let Err(e) = dashboard::serve(address, &novnc_dir) {
                eprintln!("sellaite: {e}");
                std::process::exit(EXIT_FAILURE);
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
