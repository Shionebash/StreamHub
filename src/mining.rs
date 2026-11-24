//! Channel points engine: automatic watch-time heartbeats, bonus chest
//! auto-claiming via PubSub, balance/rewards queries, and custom reward redemptions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gql::{ChannelPointsContext, CustomReward, GqlClient, StreamWatchInfo};
use crate::pubsub::{self, PubSubMessage};

const WATCH_INTERVAL: Duration = Duration::from_secs(60);
const INFO_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Bonus channel points chest available to claim.
#[derive(Clone, Debug)]
pub struct Chest {
    pub channel: String,
    pub channel_id: String,
    pub claim_id: String,
    pub points: u64,
}

/// Events sent from the background channel points worker to the UI.
#[derive(Clone, Debug)]
pub enum Event {
    Connected(bool),
    PubSubAuthRejected(String),
    Points {
        channel: String,
        balance: u64,
        earned: u64,
    },
    ChannelContext {
        channel: String,
        context: ChannelPointsContext,
    },
    WatchTick {
        channel: String,
        ticks: u64,
    },
    Chest(Chest),
    Claimed {
        channel: String,
        balance: i64,
    },
    ClaimFailed(String),
    Redeemed {
        channel: String,
        reward_title: String,
        cost: u64,
    },
    RedeemFailed {
        channel: String,
        reward_title: String,
        error: String,
    },
    DropProgress {
        drop_id: String,
        channel: String,
        current: u64,
        required: u64,
    },
    DropClaim {
        instance: String,
        channel: String,
    },
}

/// Commands sent from the UI to the background channel points worker.
pub enum Command {
    /// Start sending `minute-watched` heartbeats for an actively watched channel.
    Watch(String),
    /// Stop sending `minute-watched` heartbeats for a channel.
    Unwatch(String),
    /// Fetch channel points balance, pending bonus claim, and custom rewards.
    RefreshChannel(String),
    /// Manually claim a pending bonus chest.
    Claim(Chest),
    /// Redeem a custom channel points reward.
    Redeem {
        channel: String,
        channel_id: String,
        reward: CustomReward,
        text_input: Option<String>,
    },
    /// Refresh only the balance of a channel.
    RefreshBalance(String),
    /// Stop the worker thread and its PubSub connection.
    Stop,
}

struct WatchedState {
    info: Option<StreamWatchInfo>,
    last_watch: Option<Instant>,
    last_info_refresh: Instant,
    last_context_refresh: Instant,
    ticks: u64,
}

/// Spawns the background channel points worker thread.
pub fn spawn(
    token: String,
    user_id: String,
    events: Sender<Event>,
    commands: Receiver<Command>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("mining".into())
        .spawn(move || run(token, user_id, events, commands, stop))
        .expect("failed to spawn channel points thread")
}

fn run(
    token: String,
    user_id: String,
    events: Sender<Event>,
    commands: Receiver<Command>,
    stop: Arc<AtomicBool>,
) {
    let gql = GqlClient::new(&token).ok();

    let (tx, rx) = std::sync::mpsc::channel::<PubSubMessage>();
    let (status_tx, status_rx) = std::sync::mpsc::channel::<pubsub::Status>();
    let _pubsub = pubsub::spawn(
        token.clone(),
        user_id.clone(),
        tx,
        status_tx,
        Arc::clone(&stop),
    );

    let mut auto_claim = true;
    let mut watched: HashMap<String, WatchedState> = HashMap::new();
    let mut channel_ids: HashMap<String, String> = HashMap::new();

    while !stop.load(Ordering::Relaxed) {
        // PubSub connection status.
        while let Ok(status) = status_rx.try_recv() {
            match status {
                pubsub::Status::Connected => {
                    let _ = events.send(Event::Connected(true));
                }
                pubsub::Status::Disconnected => {
                    let _ = events.send(Event::Connected(false));
                }
                pubsub::Status::AuthRejected(err) => {
                    let _ = events.send(Event::PubSubAuthRejected(err));
                }
            }
        }

        // PubSub events.
        while let Ok(message) = rx.try_recv() {
            if message.topic.starts_with("community-points-user-v1") {
                handle_points(
                    &message.payload,
                    &events,
                    &gql,
                    &mut auto_claim,
                    &channel_ids,
                );
            } else if message.topic.starts_with("user-drop-events") {
                handle_drops(&message.payload, &events);
            }
        }

        // Periodic minute-watched heartbeats for all actively watched channels.
        if let Some(client) = &gql {
            let now = Instant::now();
            for (login, state) in watched.iter_mut() {
                let due = state
                    .last_watch
                    .map(|t| now.duration_since(t) >= WATCH_INTERVAL)
                    .unwrap_or(true);
                if due {
                    let need_info = state.info.is_none()
                        || now.duration_since(state.last_info_refresh) >= INFO_REFRESH_INTERVAL;
                    if need_info {
                        state.last_info_refresh = now;
                        if let Ok(info) = client.stream_watch_info(login) {
                            if let Some(ref details) = info {
                                channel_ids.insert(details.channel_id.clone(), login.clone());
                            }
                            state.info = info;
                        }
                    }
                    state.last_watch = Some(now);
                    if let Some(ref info) = state.info {
                        if let Ok(true) = client.send_minute_watched(info, &user_id) {
                            state.ticks += 1;
                            let _ = events.send(Event::WatchTick {
                                channel: login.clone(),
                                ticks: state.ticks,
                            });
                        }
                    }
                }

                // Periodically resync context & catch any unclaimed chest missed by PubSub.
                if now.duration_since(state.last_context_refresh) >= INFO_REFRESH_INTERVAL {
                    state.last_context_refresh = now;
                    refresh_channel_context(
                        client,
                        login,
                        &events,
                        &mut channel_ids,
                        auto_claim,
                    );
                }
            }
        }

        // UI commands (short blocking wait instead of a fixed sleep).
        match commands.recv_timeout(Duration::from_millis(120)) {
            Ok(Command::Watch(raw)) => {
                let login = raw.trim().trim_start_matches('#').to_lowercase();
                if login.is_empty() {
                    continue;
                }
                if !watched.contains_key(&login) {
                    let now = Instant::now();
                    watched.insert(
                        login.clone(),
                        WatchedState {
                            info: None,
                            last_watch: None,
                            last_info_refresh: now - INFO_REFRESH_INTERVAL,
                            last_context_refresh: now,
                            ticks: 0,
                        },
                    );
                }
                if let Some(client) = &gql {
                    refresh_channel_context(
                        client,
                        &login,
                        &events,
                        &mut channel_ids,
                        auto_claim,
                    );
                }
            }
            Ok(Command::Unwatch(raw)) => {
                let login = raw.trim().trim_start_matches('#').to_lowercase();
                watched.remove(&login);
            }
            Ok(Command::RefreshChannel(raw)) => {
                let login = raw.trim().trim_start_matches('#').to_lowercase();
                if login.is_empty() {
                    continue;
                }
                if let Some(client) = &gql {
                    refresh_channel_context(
                        client,
                        &login,
                        &events,
                        &mut channel_ids,
                        auto_claim,
                    );
                }
            }
            Ok(Command::Claim(chest)) => {
                claim(&gql, &chest, &events);
            }
            Ok(Command::Redeem {
                channel,
                channel_id,
                reward,
                text_input,
            }) => {
                let Some(client) = &gql else {
                    let _ = events.send(Event::RedeemFailed {
                        channel,
                        reward_title: reward.title,
                        error: "no se pudo preparar el cliente GQL".to_string(),
                    });
                    continue;
                };
                match client.redeem_custom_reward(&channel_id, &reward, text_input.as_deref()) {
                    Ok(_) => {
                        let _ = events.send(Event::Redeemed {
                            channel: channel.clone(),
                            reward_title: reward.title,
                            cost: reward.cost,
                        });
                        refresh_channel_context(
                            client,
                            &channel,
                            &events,
                            &mut channel_ids,
                            auto_claim,
                        );
                    }
                    Err(error) => {
                        let _ = events.send(Event::RedeemFailed {
                            channel,
                            reward_title: reward.title,
                            error,
                        });
                    }
                }
            }
            Ok(Command::RefreshBalance(login)) => {
                if let Some(client) = &gql {
                    match client.points_balance(&login) {
                        Ok(Some(balance)) => {
                            let _ = events.send(Event::Points {
                                channel: login,
                                balance: balance.max(0) as u64,
                                earned: 0,
                            });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = events.send(Event::ClaimFailed(error));
                        }
                    }
                }
            }
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn refresh_channel_context(
    client: &GqlClient,
    login: &str,
    events: &Sender<Event>,
    channel_ids: &mut HashMap<String, String>,
    auto_claim: bool,
) {
    if let Ok(context) = client.channel_points_context(login) {
        if !context.channel_id.is_empty() {
            channel_ids.insert(context.channel_id.clone(), login.to_string());
        }
        if let Some(balance) = context.balance {
            let _ = events.send(Event::Points {
                channel: login.to_string(),
                balance,
                earned: 0,
            });
        }
        let pending_claim = context.available_claim_id.clone();
        let channel_id = context.channel_id.clone();
        let _ = events.send(Event::ChannelContext {
            channel: login.to_string(),
            context,
        });
        if auto_claim {
            if let Some(claim_id) = pending_claim {
                if !channel_id.is_empty() {
                    let chest = Chest {
                        channel: login.to_string(),
                        channel_id,
                        claim_id,
                        points: 50,
                    };
                    let _ = events.send(Event::Chest(chest.clone()));
                    claim_with_client(client, &chest, events);
                }
            }
        }
    }
}

fn resolve_channel(
    data: &Value,
    channel_ids: &HashMap<String, String>,
) -> (String, String) {
    let channel_id = data
        .get("channel_id")
        .map(|value| value.to_string().trim_matches('"').to_string())
        .unwrap_or_default();
    let login = data
        .get("channel_login")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase())
        .or_else(|| channel_ids.get(&channel_id).cloned())
        .unwrap_or_else(|| format!("id_{channel_id}"));
    (login, channel_id)
}

fn handle_points(
    payload: &Value,
    events: &Sender<Event>,
    gql: &Option<GqlClient>,
    auto_claim: &mut bool,
    channel_ids: &HashMap<String, String>,
) {
    let kind = payload.get("type").and_then(|value| value.as_str()).unwrap_or("");
    let data = payload.get("data");
    match kind {
        "claim-available" => {
            let claim_data = data.and_then(|value| value.get("claim"));
            let Some(claim_data) = claim_data else { return };
            let (channel, channel_id) = resolve_channel(claim_data, channel_ids);
            let claim_id = claim_data
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let points = claim_data
                .pointer("/point_gain/total_points")
                .and_then(|value| value.as_u64())
                .unwrap_or(50);
            let chest = Chest {
                channel,
                channel_id,
                claim_id,
                points,
            };
            let _ = events.send(Event::Chest(chest.clone()));
            if *auto_claim {
                claim(gql, &chest, events);
            }
        }
        "points-earned" => {
            let Some(data) = data else { return };
            let (channel, _) = resolve_channel(data, channel_ids);
            let balance = data
                .pointer("/balance/balance")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let earned = data
                .pointer("/point_gain/total_points")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let _ = events.send(Event::Points {
                channel,
                balance,
                earned,
            });
        }
        _ => {}
    }
}

fn handle_drops(payload: &Value, events: &Sender<Event>) {
    let kind = payload.get("type").and_then(|value| value.as_str()).unwrap_or("");
    let Some(data) = payload.get("data") else { return };
    match kind {
        "drop-progress" => {
            let drop_id = data
                .get("drop_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let channel = data
                .get("channel_login")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let current = data
                .get("current_progress_min")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let required = data
                .get("required_minutes_watched")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let _ = events.send(Event::DropProgress {
                drop_id,
                channel,
                current,
                required,
            });
        }
        "drop-claim" => {
            let instance = data
                .get("drop_instance_id")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let channel = data
                .get("channel_login")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let _ = events.send(Event::DropClaim { instance, channel });
        }
        _ => {}
    }
}

fn claim(gql: &Option<GqlClient>, chest: &Chest, events: &Sender<Event>) {
    let Some(client) = gql else {
        let _ = events.send(Event::ClaimFailed(
            "no se pudo preparar el cliente GQL".to_string(),
        ));
        return;
    };
    claim_with_client(client, chest, events);
}

fn claim_with_client(client: &GqlClient, chest: &Chest, events: &Sender<Event>) {
    match client.claim_points(&chest.channel_id, &chest.claim_id) {
        Ok(balance) => {
            let _ = events.send(Event::Claimed {
                channel: chest.channel.clone(),
                balance,
            });
        }
        Err(error) => {
            let _ = events.send(Event::ClaimFailed(error));
        }
    }
}
