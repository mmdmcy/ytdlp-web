mod app;
mod features;
mod integrations;
mod interfaces;
mod persistence;
mod security;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    interfaces::cli::run().await
}
