use std::collections::HashMap;
use std::sync::Arc;

use serenity::model::id::{ChannelId, GuildId, UserId};
use serenity::{async_trait, CacheAndHttp};
use songbird::Songbird;
use thiserror::Error;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::task_handle::{DeleteMessageTask, TaskRoute};

use crate::bots::BotMap;
use crate::config::Config;
use crate::event_handler::{Event, EventHandler, GuildEventHandler};
use crate::guild::message_manager::SearchMessage;
use crate::guild::player_manager::{PlayerManager, PlayerRequest};
use crate::guild::scheduler::GuildScheduler;
use crate::lavalink_handler::{GuildLavalinkHandler, LavalinkEvent};
use crate::lavalink_supervisor::LavalinkSupervisor;
use arc_swap::ArcSwap;
use serenity::model::prelude::Message;
use std::ops::Not;

mod message_manager;
pub mod player_manager;
mod scheduler;

#[derive(Clone)]
pub struct ReciprocityGuild {
    id: GuildId,
    channel: ChannelId,
    bots: Arc<BotMap>,
    event_handler: EventHandler,
    config: Arc<Config>,

    scheduler: GuildScheduler,
    player_manager: Arc<PlayerManager>,
}

impl ReciprocityGuild {
    pub fn new(
        id: GuildId,
        bots: Arc<BotMap>,
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

        let scheduler = GuildScheduler::new(id, channel, bots.clone());
        let player_manager = Arc::new(PlayerManager::new(id, bots.clone(), lavalink_supervisor));

        let mut guild = ReciprocityGuild {
            id,
            channel,
            bots,
            event_handler,
            config,
            scheduler,
            player_manager,
        };

        Ok(guild)
    }

    async fn handle_new_message(&self, message: Message) -> Result<(), ()> {
        let user = message.author.id;
        if message.channel_id.eq(&self.channel).not() {
            return Err(());
        }
        if self.bots.contains_id(&user) {
            return Err(());
        }
        self.scheduler
            .process_enqueue(DeleteMessageTask {
                channel: message.channel_id,
                message: message.id,
            })
            .await
            .ok();
        let voice_channel = self
            .bots
            .get_user_voice_state(&user, &self.id)
            .await
            .map(|v| v.channel_id)
            .flatten()
            .ok_or(())?;
        let (bot, tracks) = self
            .player_manager
            .search(voice_channel, message.content)
            .await
            .map_err(|_| ())?;
        let track = SearchMessage::search(
            self.bots.get_bot_by_id(bot).ok_or(())?.http().clone(),
            self.channel,
            tracks,
            user,
            self.event_handler
                .get_shard_sender(self.id, bot)
                .await
                .ok_or(())?,
            self.scheduler.clone(),
        )
        .await
        .map_err(|_| ())?;
        self.player_manager
            .request(PlayerRequest::Enqueue(track, voice_channel))
            .await
            .map_err(|_| ())
    }
}

#[async_trait]
impl GuildEventHandler for ReciprocityGuild {
    async fn run(&self, event: Event) {
        match event {
            Event::NewMessage(message) => {
                self.handle_new_message(message).await.ok();
            }
            Event::DeleteMessage(_, _) => {
                todo!("Check if MainMessage was deleted")
            }
            Event::Resume(_, _) => {
                todo!("Check if anything was missed")
            }
            Event::VoiceUpdate(old, _) => {
                if let Some(voice_state) = old {
                    if let Some(channel) = voice_state.channel_id {
                        if !self.bots.user_in_channel(&channel, &self.id).await {
                            self.player_manager.leave(channel).await.ok();
                        }
                    }
                }
            }
        }
    }
}
#[derive(Debug, Error)]
pub enum ReciprocityGuildError {
    #[error("Guild was not found in config: {0:?}")]
    GuildNotInConfig(GuildId),
    //#[error("PlayerMap Error occurred: {0:?}")]
    //PlayerMap(PlayerMapError),
}

#[async_trait]
impl GuildLavalinkHandler for ReciprocityGuild {
    async fn run(&self, event: LavalinkEvent) {
        self.player_manager.handle_player_event(event).await.ok();
    }
}
