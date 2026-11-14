//! gh-token CLI entry point.

use gh_token_client::Cli;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(err) = gh_token_client::run(&cli).await {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
