use crate::context::GuildEventHandler;
use lavalink_rs::gateway::LavalinkEventHandler;
use lavalink_rs::model::{PlayerUpdate, Stats, TrackFinish, TrackStart};
use lavalink_rs::LavalinkClient;
use serenity::async_trait;
use serenity::model::id::GuildId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug)]
pub enum LavalinkEvent {
    Update(PlayerUpdate),
    Start(TrackStart),
    Finish(TrackFinish),
}

#[derive(Clone)]
pub struct LavalinkHandler {
    guilds: Arc<RwLock<HashMap<GuildId, Arc<dyn GuildEventHandler>>>>,
}

impl LavalinkHandler {
    pub fn new() -> LavalinkHandler {
        LavalinkHandler {
            guilds: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_guild(&self, guild: GuildId, event_handler: Arc<dyn GuildEventHandler>) {
        self.guilds.write().await.insert(guild, event_handler);
    }
}

#[async_trait]
impl LavalinkEventHandler for LavalinkHandler {
    async fn stats(&self, _: LavalinkClient, _: Stats) {
        //TODO maybe handle
    }

    async fn player_update(&self, client: LavalinkClient, event: PlayerUpdate) {
        if let Some(handler) = self.guilds.read().await.get(&GuildId(event.guild_id)) {
            handler.lavalink(LavalinkEvent::Update(event), client).await;
        }
    }

    async fn track_start(&self, client: LavalinkClient, event: TrackStart) {
        if let Some(handler) = self.guilds.read().await.get(&GuildId(event.guild_id)) {
            handler.lavalink(LavalinkEvent::Start(event), client).await;
        }
    }

    async fn track_finish(&self, client: LavalinkClient, event: TrackFinish) {
        if let Some(handler) = self.guilds.read().await.get(&GuildId(event.guild_id)) {
            handler.lavalink(LavalinkEvent::Finish(event), client).await;
        }
    }
}
