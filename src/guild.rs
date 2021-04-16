use std::collections::HashMap;
use std::sync::Arc;

use serenity::async_trait;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use thiserror::Error;

use crate::bots::BotMap;
use crate::config::Config;
use crate::context::{Context, GuildEventHandler};
use crate::event_handler::EventHandler;
use crate::guild::message_manager::MessageError;
use crate::guild::player_manager::PlayerManager;
use crate::guild::scheduler::GuildScheduler;
use crate::lavalink_handler::LavalinkEvent;
use lavalink_rs::LavalinkClient;
use serenity::model::event::ResumedEvent;
use serenity::model::prelude::{Message, VoiceState};
use std::time::Instant;

pub mod message_manager;
pub mod player_manager;
pub mod scheduler;

#[derive(Clone)]
pub struct ReciprocityGuild(Context);

impl ReciprocityGuild {
    pub fn new(
        id: GuildId,
        bots: Arc<BotMap>,
        event_handler: EventHandler,
        lavalink: Arc<HashMap<UserId, LavalinkClient>>,
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
        let player_manager = Arc::new(PlayerManager::new(id, bots.clone(), lavalink));

        let guild = ReciprocityGuild {
            0: Context {
                id,
                channel,
                bots,
                event_handler,
                config,
                scheduler,
                player_manager,
            },
        };

        Ok(guild)
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
impl GuildEventHandler for ReciprocityGuild {
    async fn new_message(&self, _message: Message) {
        todo!()
    }

    async fn deleted_message(&self, _channel: ChannelId, _message: MessageId) {
        todo!()
    }

    async fn resume(&self, _time: Instant, _event: ResumedEvent) {
        todo!()
    }

    async fn voice_update(&self, _old_voice_state: Option<VoiceState>, _new_voice_state: VoiceState) {
        todo!()
    }

    async fn lavalink(&self, _event: LavalinkEvent, _client: LavalinkClient) {
        todo!()
    }

    async fn main_message_error(&self, _error: MessageError) {
        todo!()
    }

    async fn player_status_changed(&self) {
        todo!()
    }

    async fn main_message_emote_check(&self) {
        todo!()
    }
}
