#![recursion_limit = "256"]

mod bot;
mod bridge;
mod connection;
mod crypto;

use clap::Parser;
use tracing::{error, info};

use crate::bot::{BotConfig, MatrixBot, MatrixEvent};
use crate::bridge::{input_to_swp, parse_matrix_input, CollectorAction, ResponseCollector};
use crate::connection::{spawn_connection, ConnCommand, ConnEvent};

#[derive(Parser)]
#[command(name = "shore-matrix", about = "Matrix bridge for Shore")]
struct Args {
    /// Matrix homeserver URL
    #[arg(long, env = "MATRIX_HOMESERVER")]
    homeserver: String,

    /// Matrix user ID (e.g. @bot:example.com)
    #[arg(long, env = "MATRIX_USER_ID")]
    user_id: String,

    /// Matrix access token
    #[arg(long, env = "MATRIX_ACCESS_TOKEN")]
    access_token: Option<String>,

    /// Matrix password (used if no access token)
    #[arg(long, env = "MATRIX_PASSWORD")]
    password: Option<String>,

    /// Matrix device ID
    #[arg(long, env = "MATRIX_DEVICE_ID")]
    device_id: Option<String>,

    /// Trusted user for automatic SAS verification
    #[arg(long, env = "MATRIX_TRUSTED_USER")]
    trusted_user: Option<String>,

    /// Daemon socket path or address
    #[arg(long)]
    socket: Option<String>,

    /// Daemon config path for discovery
    #[arg(long)]
    config: Option<String>,

    /// Path for Matrix state and crypto store
    #[arg(long, default_value = "shore_matrix_state")]
    store_path: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("shore_matrix=info".parse()?),
        )
        .init();

    let args = Args::parse();

    // Build Matrix client
    let bot_config = BotConfig {
        homeserver: args.homeserver.clone(),
        user_id: args.user_id.clone(),
        access_token: args.access_token.clone(),
        password: args.password.clone(),
        device_id: args.device_id.clone(),
        store_path: args.store_path.clone(),
    };
    let (bot, mut matrix_rx) = MatrixBot::new(&bot_config).await?;

    // Set up SAS verification for trusted user
    if let Some(ref trusted) = args.trusted_user {
        crypto::setup_verification(&bot.client, trusted)?;
    }

    // Start Matrix sync
    bot.start_sync();

    // Connect to Shore daemon via SWP
    let (daemon_tx, mut daemon_rx) = spawn_connection(args.socket, args.config);

    let mut collector = ResponseCollector::new();
    let mut active_room: Option<matrix_sdk::ruma::OwnedRoomId> = None;

    info!("shore-matrix bridge running");

    loop {
        tokio::select! {
            biased;

            // Matrix events (user messages from rooms)
            Some(event) = matrix_rx.recv() => {
                match event {
                    MatrixEvent::Message { room_id, text, .. } => {
                        active_room = Some(room_id);
                        let input = parse_matrix_input(&text);
                        let swp_msg = input_to_swp(&input);
                        if daemon_tx.send(ConnCommand::Send(swp_msg)).await.is_err() {
                            error!("daemon connection dropped");
                        }
                    }
                }
            }

            // Daemon events (responses from shore-daemon)
            Some(event) = daemon_rx.recv() => {
                match event {
                    ConnEvent::Connected { server_name, .. } => {
                        info!("connected to daemon: {server_name}");
                    }
                    ConnEvent::Disconnected(reason) => {
                        info!("daemon disconnected: {reason}");
                    }
                    ConnEvent::Message(msg) => {
                        let action = collector.feed(&msg);
                        if let Some(ref room_id) = active_room {
                            match action {
                                CollectorAction::StartTyping => {
                                    bot.set_typing(room_id, true).await;
                                }
                                CollectorAction::SendMessage { text, images } => {
                                    bot.set_typing(room_id, false).await;
                                    for img in &images {
                                        bot.send_image(
                                            room_id,
                                            &img.path,
                                            img.caption.as_deref(),
                                        ).await;
                                    }
                                    bot.send_text(room_id, &text).await;
                                }
                                CollectorAction::SendCommandOutput { name, data } => {
                                    let msg = format!("**{name}**\n```\n{data}\n```");
                                    bot.send_text(room_id, &msg).await;
                                }
                                CollectorAction::SendError(err) => {
                                    bot.send_text(room_id, &format!("Error: {err}")).await;
                                }
                                CollectorAction::SendPush(text) => {
                                    bot.send_text(room_id, &text).await;
                                }
                                CollectorAction::None => {}
                            }
                        }
                    }
                }
            }
        }
    }
}
