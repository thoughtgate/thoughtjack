//! ThoughtJack - Adversarial MCP server for security testing

use clap::Parser;

#[derive(Parser)]
#[command(name = "thoughtjack")]
#[command(version = "0.1.0")]
#[command(about = "Adversarial MCP server for security testing")]
struct Cli {
    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() {
    let _cli = Cli::parse();
    println!("ThoughtJack v0.1.0");
}
