use crate::config::Config;
use crate::event_handler::{Event, GuildEventHandler};
use crate::lavalink_handler::{GuildLavalinkHandler, LavalinkEvent};
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::player::Player;
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
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct ReciprocityGuild {
    id: GuildId,
    channel: ChannelId,
    bots: HashMap<UserId, (Arc<Http>, Arc<Songbird>)>,
    scheduler: TaskScheduler,
    lavalink_supervisor: LavalinkSupervisor,
    player: Arc<HashMap<UserId, RwLock<Option<Player>>>>,
    config: Config,
}

type SchedulerRun = Pin<Box<dyn Future<Output = SchedulerError> + Send>>;

impl ReciprocityGuild {
    pub fn new(
        id: GuildId,
        bots: HashMap<UserId, (Arc<Http>, Arc<Songbird>)>,
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

        let player: Arc<HashMap<_, _>> = Arc::new(bots.keys().map(|id| (*id, RwLock::new(None))).collect());

        Ok((
            ReciprocityGuild {
                id,
                channel,
                bots,
                scheduler,
                lavalink_supervisor,
                player,
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
}

#[async_trait]
impl GuildLavalinkHandler for ReciprocityGuild {
    async fn run(&self, event: LavalinkEvent) {
        unimplemented!()
    }
}
