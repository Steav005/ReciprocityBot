use lavalink_rs::gateway::LavalinkEventHandler;
use lavalink_rs::model::{PlayerUpdate, Stats, TrackFinish, TrackStart};
use lavalink_rs::LavalinkClient;
use serenity::async_trait;
use serenity::model::id::GuildId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub enum LavalinkEvent {
    //Stats(Stats),
    PlayerUpdate(PlayerUpdate, LavalinkClient),
    TrackStart(TrackStart, LavalinkClient),
    TrackFinish(TrackFinish, LavalinkClient),
}

impl LavalinkEvent {
    pub fn get_client(&self) -> &LavalinkClient {
        match self {
            LavalinkEvent::PlayerUpdate(_, client) => client,
            LavalinkEvent::TrackStart(_, client) => client,
            LavalinkEvent::TrackFinish(_, client) => client,
        }
    }
}

#[derive(Clone)]
pub struct LavalinkHandler {
    guilds: Arc<RwLock<HashMap<GuildId, Arc<dyn GuildLavalinkHandler>>>>,
}

impl LavalinkHandler {
    pub fn new() -> LavalinkHandler {
        LavalinkHandler {
            guilds: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_guild(&self, guild: GuildId, event_handler: Arc<dyn GuildLavalinkHandler>) {
        self.guilds.write().await.insert(guild, event_handler);
    }
}

#[async_trait]
impl LavalinkEventHandler for LavalinkHandler {
    async fn stats(&self, _: LavalinkClient, _: Stats) {
        todo!()
    }

    async fn player_update(&self, client: LavalinkClient, event: PlayerUpdate) {
        if let Some(handler) = self.guilds.read().await.get(&GuildId(event.guild_id)) {
            handler
                .run(LavalinkEvent::PlayerUpdate(event, client))
                .await;
        }
    }

    async fn track_start(&self, client: LavalinkClient, event: TrackStart) {
        if let Some(handler) = self.guilds.read().await.get(&GuildId(event.guild_id)) {
            handler.run(LavalinkEvent::TrackStart(event, client)).await;
        }
    }

    async fn track_finish(&self, client: LavalinkClient, event: TrackFinish) {
        if let Some(handler) = self.guilds.read().await.get(&GuildId(event.guild_id)) {
            handler.run(LavalinkEvent::TrackFinish(event, client)).await;
        }
    }
}

#[async_trait]
pub trait GuildLavalinkHandler: Send + Sync {
    async fn run(&self, event: LavalinkEvent);
}
