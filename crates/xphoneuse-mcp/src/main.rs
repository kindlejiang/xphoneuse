use anyhow::Result;
use clap::Parser;
use rmcp::ServiceExt;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "xphoneuse-mcp", about = "MCP server for Android device control via XPhoneUse")]
struct Args {
    #[arg(long, env = "SCRCPY_SERVER_PATH")]
    scrcpy_server: Option<String>,

    #[arg(long)]
    sse: bool,

    #[arg(long, default_value = "7070")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args = Args::parse();

    if args.sse {
        let ct = CancellationToken::new();
        // Standalone mode has no app to share an AgentInput client, live
        // mirror frames, or iOS sessions with.
        let agent_input = std::sync::Arc::new(std::sync::Mutex::new(None));
        xphoneuse_mcp::start_sse_server(args.port, args.scrcpy_server, ct, agent_input, Default::default()).await
    } else {
        let server = xphoneuse_mcp::server::XPhoneUseMcp::new(args.scrcpy_server);
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    }
}
