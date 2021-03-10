use crate::config::Config;
use crate::event_handler::{Event, EventHandler, GuildEventHandler};
use crate::lavalink_handler::{GuildLavalinkHandler, LavalinkEvent};
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::player_manager::{PlayerManager, PlayerMapError};
use crate::scheduler::GuildScheduler;
use crate::task_handle::DeleteMessageTask;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use serenity::{async_trait, CacheAndHttp};
use songbird::Songbird;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone)]
pub struct ReciprocityGuild {
    id: GuildId,
    channel: ChannelId,
    bots: HashMap<UserId, (Arc<CacheAndHttp>, Arc<Songbird>)>,
    event_handler: EventHandler,
    scheduler: GuildScheduler,
    player_map: PlayerManager,
    config: Config,
}

impl ReciprocityGuild {
    pub fn new(
        id: GuildId,
        bots: HashMap<UserId, (Arc<CacheAndHttp>, Arc<Songbird>)>,
        event_handler: EventHandler,
        lavalink_supervisor: LavalinkSupervisor,
        config: Config,
    ) -> Result<ReciprocityGuild, ReciprocityGuildError> {
        let channel = config
            .guilds
            .values()
            .map(|guild| (guild.guild_id, ChannelId(guild.channel_id)))
            .find(|(guild_id, _)| *guild_id == id.0)
            .ok_or(ReciprocityGuildError::GuildNotInConfig(id))?
            .1;

        let scheduler = GuildScheduler::new(
            id,
            channel,
            bots.values()
                .map(|(cache_http, _)| cache_http.clone())
                .collect(),
        );

        let player_map = PlayerManager::new(id, bots.clone(), lavalink_supervisor);

        Ok(ReciprocityGuild {
            id,
            channel,
            bots,
            event_handler,
            scheduler,
            player_map,
            config,
        })
    }
}

#[async_trait]
impl GuildEventHandler for ReciprocityGuild {
    async fn run(&self, event: Event) {
        match event {
            Event::NewMessage(_) => {}
            Event::DeleteMessage(_, _) => {}
            Event::Resume(_, _) => {}
            Event::VoiceUpdate(_, _) => {}
        }

        todo!()
    }
}
#[derive(Debug, Error)]
pub enum ReciprocityGuildError {
    #[error("Guild was not found in config: {0:?}")]
    GuildNotInConfig(GuildId),
    #[error("PlayerMap Error occurred: {0:?}")]
    PlayerMap(PlayerMapError),
}

#[async_trait]
impl GuildLavalinkHandler for ReciprocityGuild {
    async fn run(&self, event: LavalinkEvent) {
        self.player_map.run(event).await;
    }
}
