use clap::{Parser, Subcommand};
use log::info;
use std::time::Duration;

use zel_core::protocol::RpcServerBuilder;
use zel_core::IrohBundle;

mod audio_player;
mod audio_source;
mod broadcaster;
mod listener;
mod service;

use broadcaster::RadioBroadcaster;
use listener::RadioListener;
use service::{RadioServiceClient, RadioServiceServer};

#[derive(Parser)]
#[command(name = "zelfm")]
#[command(about = "P2P Internet Radio - Simplified Architecture")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start broadcasting a radio station
    Broadcast {
        /// Station name
        #[arg(short, long, default_value = "ZelFM Demo")]
        name: String,

        /// OGG Vorbis audio file to broadcast (will loop)
        #[arg(short, long)]
        file: String,
    },

    /// Listen to a radio station
    Listen {
        /// Broadcaster node ID
        #[arg(short, long)]
        node_id: String,

        /// Max listening duration in seconds (optional)
        #[arg(short, long)]
        duration: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Broadcast { name, file } => broadcast_station(name, file).await?,
        Commands::Listen { node_id, duration } => listen_to_station(node_id, duration).await?,
    }

    Ok(())
}

async fn broadcast_station(name: String, file_path: String) -> anyhow::Result<()> {
    println!("=== ZelFM Broadcaster ===\n");

    // Create broadcaster
    let (broadcaster, pcm_tx) = RadioBroadcaster::new(
        name.clone(),
        format!("Broadcasting {}", file_path),
        44100, // 44.1 kHz
        2,     // Stereo
    );

    // Start audio decoder thread
    let file_clone = file_path.clone();
    std::thread::spawn(move || {
        if let Err(e) = audio_source::audio_decode_loop(&file_clone, pcm_tx) {
            eprintln!("[Audio] Error: {}", e);
        }
    });

    // Setup Iroh
    let mut server_bundle = IrohBundle::builder(None).await?;
    let node_id = server_bundle.endpoint().id();

    println!("Node ID: {}", node_id);
    println!("Station: {}", name);
    println!("File: {}", file_path);
    println!("\nWaiting for listeners...\n");

    // Build server
    let server =
        RpcServerBuilder::new(b"zelfm/1", server_bundle.endpoint().clone()).service("radio");

    let server = broadcaster.into_service_builder(server).build().build();
    let server_bundle = server_bundle.accept(b"zelfm/1", server).finish().await;

    // Run until Ctrl+C
    tokio::signal::ctrl_c().await?;
    println!("\nShutting down...");
    server_bundle.shutdown(Duration::from_secs(1)).await?;

    Ok(())
}

async fn listen_to_station(node_id_str: String, duration: Option<u64>) -> anyhow::Result<()> {
    println!("=== ZelFM Listener ===\n");

    let node_id: iroh::PublicKey = node_id_str.parse()?;
    let client_bundle = IrohBundle::builder(None).await?.finish().await;

    info!("[Listener] Connecting to {}", node_id);
    let connection = client_bundle.endpoint.connect(node_id, b"zelfm/1").await?;

    let rpc_client = zel_core::protocol::client::RpcClient::new(connection).await?;
    let radio_client = RadioServiceClient::new(rpc_client);
    let listener = RadioListener::new(radio_client);

    listener.get_station_info().await?;
    listener.listen(duration).await?;

    println!("\nDisconnected.");
    Ok(())
}
