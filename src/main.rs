mod desktop;
mod input;
mod keymap;
mod mcp;

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
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP over stdio
    Serve,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Mcp {
            command: McpCommand::Serve,
        } => {
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            if let Err(e) = mcp::Server::default().serve(stdin, stdout) {
                eprintln!("sellaite: {e}");
                std::process::exit(1);
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
