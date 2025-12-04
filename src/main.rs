use clap::{Args, Parser, Subcommand};
use log::info;
use std::time::Duration;

use zel_core::protocol::RpcServerBuilder;
use zel_core::IrohBundle;

mod audio_player;
mod audio_source;
mod broadcaster;
mod devices;
mod listener;
mod service;

use audio_source::{AudioSource, FileSource};
use broadcaster::RadioBroadcaster;
use listener::RadioListener;
use service::{RadioServiceClient, RadioServiceServer};

#[cfg(feature = "live-input")]
use audio_source::LiveSource;

#[derive(Parser)]
#[command(name = "zelfm")]
#[command(about = "P2P Internet Radio - File & Live Streaming")]
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

        #[command(flatten)]
        source: AudioSourceArgs,
    },

    /// List available input devices
    #[cfg(feature = "live-input")]
    ListDevices,

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

#[derive(Args)]
#[group(required = true, multiple = false)]
struct AudioSourceArgs {
    /// Audio file to broadcast (loops)
    #[arg(short, long)]
    file: Option<String>,

    /// Live input device name (partial match, use list-devices to see options)
    #[cfg(feature = "live-input")]
    #[arg(short, long)]
    input: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Broadcast { name, source } => broadcast_station(name, source).await?,

        #[cfg(feature = "live-input")]
        Commands::ListDevices => {
            devices::list_input_devices()?;
        }

        Commands::Listen { node_id, duration } => listen_to_station(node_id, duration).await?,
    }

    Ok(())
}

async fn broadcast_station(name: String, source: AudioSourceArgs) -> anyhow::Result<()> {
    println!("=== ZelFM Broadcaster ===\n");

    // Create broadcaster
    let (broadcaster, pcm_tx) = RadioBroadcaster::new(
        name.clone(),
        "Live P2P Radio Stream",
        44100, // Target: 44.1 kHz
        2,     // Target: Stereo
    );

    // Determine and start audio source
    std::thread::spawn(move || {
        let result = if let Some(file_path) = source.file {
            // File source
            println!("Source: File ({})", file_path);
            let audio_source = FileSource::new(file_path);
            audio_source.start(pcm_tx)
        } else {
            #[cfg(feature = "live-input")]
            if let Some(device_name) = source.input {
                // Live input source
                println!("Source: Live Input ({})", device_name);
                let audio_source = LiveSource::new(Some(device_name));
                audio_source.start(pcm_tx)
            } else {
                Err(anyhow::anyhow!("No audio source specified"))
            }

            #[cfg(not(feature = "live-input"))]
            Err(anyhow::anyhow!("No audio source specified"))
        };

        if let Err(e) = result {
            eprintln!("[Audio] Error: {}", e);
        }
    });

    // Setup Iroh
    let mut server_bundle = IrohBundle::builder(None).await?;
    let node_id = server_bundle.endpoint().id();

    println!("Node ID: {}", node_id);
    println!("Station: {}", name);
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
