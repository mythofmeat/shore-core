#![recursion_limit = "256"]

use std::collections::HashMap;

use clap::Parser;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use tracing::{error, info};

use shore_matrix::bot::{BotConfig, MatrixBot, MatrixEvent};
use shore_matrix::bridge::{input_to_swp, parse_matrix_input, CollectorAction, MatrixInput, ResponseCollector};
use shore_matrix::connection::{spawn_connection, ConnCommand, ConnEvent};
use shore_matrix::crypto;
use shore_matrix::rooms::RoomManager;

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

    // Per-room state for cross-room isolation
    let mut room_manager = RoomManager::new();
    let mut collectors: HashMap<OwnedRoomId, ResponseCollector> = HashMap::new();
    let mut active_room: Option<OwnedRoomId> = None;
    let mut known_characters: Vec<String> = Vec::new();

    info!("shore-matrix bridge running");

    loop {
        tokio::select! {
            biased;

            // Matrix events (user messages from rooms)
            Some(event) = matrix_rx.recv() => {
                match event {
                    MatrixEvent::Message { room_id, text, .. } => {
                        let input = parse_matrix_input(&text);
                        match &input {
                            // Bridge-local !bind command
                            MatrixInput::Command { name, args } if name == "bind" => {
                                handle_bind(
                                    &bot, &mut room_manager, &known_characters,
                                    &room_id, args,
                                ).await;
                            }
                            _ => {
                                active_room = Some(room_id);
                                let swp_msg = input_to_swp(&input);
                                if daemon_tx.send(ConnCommand::Send(swp_msg)).await.is_err() {
                                    error!("daemon connection dropped");
                                }
                            }
                        }
                    }
                    MatrixEvent::Image { room_id, path, body, .. } => {
                        active_room = Some(room_id);
                        let input = MatrixInput::Image {
                            path,
                            caption: Some(body),
                        };
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
                    ConnEvent::Connected { server_name, characters, .. } => {
                        info!("connected to daemon: {server_name}");
                        known_characters = characters.iter().map(|c| c.name.clone()).collect();

                        // Sync avatar for the first available character
                        if let Some(first) = known_characters.first() {
                            bot.sync_avatar(first).await;
                        }
                    }
                    ConnEvent::Disconnected(reason) => {
                        info!("daemon disconnected: {reason}");
                    }
                    ConnEvent::Message(msg) => {
                        // For push messages, prefer the character's bound room
                        let target = if matches!(msg, shore_protocol::server_msg::ServerMessage::NewMessage(_)) {
                            push_target(&known_characters, &room_manager)
                                .or(active_room.clone())
                        } else {
                            active_room.clone()
                        };

                        if let Some(ref room_id) = target {
                            let collector = collectors
                                .entry(room_id.clone())
                                .or_default();
                            let action = collector.feed(&msg);
                            dispatch_action(&bot, room_id, action).await;
                        }
                    }
                }
            }
        }
    }
}

/// Handle the bridge-local `!bind <character>` command.
async fn handle_bind(
    bot: &MatrixBot,
    room_manager: &mut RoomManager,
    known_characters: &[String],
    room_id: &OwnedRoomId,
    args: &serde_json::Value,
) {
    let char_name = args
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if char_name.is_empty() {
        // Show current bindings
        let mut lines = vec!["**Room bindings:**".to_string()];
        let mut any = false;
        for (character, bound_room) in room_manager.bindings() {
            lines.push(format!("- **{character}** → `{bound_room}`"));
            any = true;
        }
        if !any {
            lines.push("_No rooms bound yet._".into());
        }
        if !known_characters.is_empty() {
            lines.push(format!(
                "\nAvailable characters: {}",
                known_characters.join(", ")
            ));
        }
        bot.send_text(room_id, &lines.join("\n")).await;
        return;
    }

    if known_characters.is_empty() {
        bot.send_text(room_id, "Not connected to daemon yet").await;
        return;
    }

    if known_characters.iter().any(|c| c == char_name) {
        room_manager.bind(room_id.as_str(), char_name);
        bot.send_text(room_id, &format!("Bound this room to **{char_name}**"))
            .await;
    } else {
        let available = known_characters.join(", ");
        bot.send_text(
            room_id,
            &format!("Unknown character `{char_name}`. Available: {available}"),
        )
        .await;
    }
}

/// Find the room bound to the first known character (for push messages).
fn push_target(known_characters: &[String], room_manager: &RoomManager) -> Option<OwnedRoomId> {
    for char_name in known_characters {
        if let Some(room_str) = room_manager.room_for_character(char_name) {
            if let Ok(room_id) = <&RoomId>::try_from(room_str) {
                return Some(room_id.to_owned());
            }
        }
    }
    None
}

/// Send a CollectorAction to a Matrix room.
async fn dispatch_action(bot: &MatrixBot, room_id: &OwnedRoomId, action: CollectorAction) {
    match action {
        CollectorAction::StartTyping => {
            bot.set_typing(room_id, true).await;
        }
        CollectorAction::SendMessage { text, images } => {
            bot.set_typing(room_id, false).await;
            for img in &images {
                bot.send_image(room_id, &img.path, img.caption.as_deref())
                    .await;
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
