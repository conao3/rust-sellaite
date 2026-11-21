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
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            if let Err(e) = mcp::Server::default().serve(stdin, stdout) {
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
