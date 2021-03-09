use crate::config::Config;
use crate::event_handler::{Event, EventHandler, GuildEventHandler};
use crate::lavalink_handler::{GuildLavalinkHandler, LavalinkEvent};
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::player_manager::{PlayerManager, PlayerMapError};
use crate::scheduler::{SchedulerError, TaskScheduler};
use futures::Future;
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::id::{ChannelId, GuildId, UserId};
use songbird::Songbird;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone)]
pub struct ReciprocityGuild {
    id: GuildId,
    channel: ChannelId,
    bots: HashMap<UserId, (Arc<Http>, Arc<Songbird>)>,
    event_handler: EventHandler,
    scheduler: TaskScheduler,
    player_map: PlayerManager,
    config: Config,
}

type SchedulerRun = Pin<Box<dyn Future<Output = SchedulerError> + Send>>;

impl ReciprocityGuild {
    pub fn new(
        id: GuildId,
        bots: HashMap<UserId, (Arc<Http>, Arc<Songbird>)>,
        event_handler: EventHandler,
        lavalink_supervisor: LavalinkSupervisor,
        config: Config,
    ) -> Result<(ReciprocityGuild, SchedulerRun), ReciprocityGuildError> {
        let channel = config
            .guilds
            .values()
            .map(|guild| (guild.guild_id, ChannelId(guild.channel_id)))
            .find(|(guild_id, _)| *guild_id == id.0)
            .ok_or(ReciprocityGuildError::GuildNotInConfig(id))?
            .1;

        let (scheduler, run) = TaskScheduler::new(
            id,
            channel,
            bots.values().map(|(http, _)| http.clone()).collect(),
        );

        let player_map = PlayerManager::new(bots.clone(), lavalink_supervisor);

        Ok((
            ReciprocityGuild {
                id,
                channel,
                bots,
                event_handler,
                scheduler,
                player_map,
                config,
            },
            run,
        ))
    }
}

#[async_trait]
impl GuildEventHandler for ReciprocityGuild {
    async fn run(&self, event: Event) {
        unimplemented!()
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
