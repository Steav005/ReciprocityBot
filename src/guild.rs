use std::collections::HashMap;
use std::sync::Arc;

use serenity::model::id::{ChannelId, GuildId, UserId};
use serenity::{async_trait, CacheAndHttp};
use songbird::Songbird;
use thiserror::Error;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::task_handle::TaskRoute;

use crate::config::Config;
use crate::event_handler::{Event, EventHandler, GuildEventHandler};
use crate::guild::message_manager::{
    MainMessageMutex, MessageEventHandler, MessageManager, MessageManagerEvent, SearchMessageMap,
};
use crate::guild::player_manager::{PlayerManager, PlayerMap, PlayerMapError, PlayerStateMap};
use crate::guild::scheduler::{GuildScheduler, RouteScheduler};
use crate::lavalink_handler::{GuildLavalinkHandler, LavalinkEvent};
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::BotList;
use arc_swap::ArcSwap;

mod message_manager;
pub mod player_manager;
mod scheduler;

#[derive(Clone)]
pub struct ReciprocityGuild {
    id: GuildId,
    channel: ChannelId,
    bots: BotList,
    event_handler: EventHandler,
    config: Arc<Config>,

    //Scheduler
    route_scheduler: Arc<HashMap<TaskRoute, RouteScheduler>>,

    //PlayerManager
    player_map: PlayerMap,
    player_state_map: PlayerStateMap,
    player_state_map_notify: Arc<Notify>,
    lavalink_supervisor: LavalinkSupervisor,

    //MessageManager
    search_messages: SearchMessageMap,
    main_message: MainMessageMutex,
}

impl ReciprocityGuild {
    pub fn new(
        id: GuildId,
        bots: BotList,
        event_handler: EventHandler,
        lavalink_supervisor: LavalinkSupervisor,
        config: Arc<Config>,
    ) -> Result<ReciprocityGuild, ReciprocityGuildError> {
        let channel = config
            .guilds
            .values()
            .map(|guild| (guild.guild_id, ChannelId(guild.channel_id)))
            .find(|(guild_id, _)| *guild_id == id.0)
            .ok_or(ReciprocityGuildError::GuildNotInConfig(id))?
            .1;

        let mut guild = ReciprocityGuild {
            id,
            channel,
            bots,
            event_handler,
            config,
            route_scheduler: Arc::new(HashMap::new()),
            player_map: Arc::new(ArcSwap::new(Arc::new(HashMap::new()))),
            player_state_map: Arc::new(RwLock::new(HashMap::new())),
            player_state_map_notify: Arc::new(Notify::new()),
            lavalink_supervisor,
            search_messages: Arc::new(RwLock::new(HashMap::new())),
            main_message: Arc::new(Mutex::new(None)),
        };
        guild.init_scheduler();
        guild.init_player_manager();
        guild.init_message_manager();

        Ok(guild)
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
        if let Some((bot, player_lock)) = self.get_player_lavalink(event.get_client()).await {
            let mut player_lock = player_lock.write().await;
            if let Some(player) = player_lock.as_mut() {
                match event {
                    LavalinkEvent::PlayerUpdate(update, _) => player.update(update),
                    LavalinkEvent::TrackStart(start, _) => player.track_start(start),
                    //TODO Maybe react to Error
                    LavalinkEvent::TrackFinish(finish, _) => {
                        player.track_end(finish).await.ok();
                    }
                }
            }
        }
    }
}

#[async_trait]
impl MessageEventHandler for ReciprocityGuild {
    async fn run(&self, event: MessageManagerEvent) {
        todo!()
    }
}
