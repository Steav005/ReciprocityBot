use crate::bots::BotMap;
use crate::config::Config;
use crate::event_handler::EventHandler;
use crate::guild::message_manager::{EmoteAction, MessageError};
use crate::guild::player_manager::PlayerManager;
use crate::guild::scheduler::GuildScheduler;
use crate::lavalink_handler::LavalinkEvent;
use lavalink_rs::LavalinkClient;
use serenity::async_trait;
use serenity::model::prelude::{
    ChannelId, GuildId, Message, MessageId, ResumedEvent, UserId, VoiceState,
};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub struct Context {
    pub id: GuildId,
    pub channel: ChannelId,
    pub bots: Arc<BotMap>,
    pub event_handler: EventHandler,
    pub config: Arc<Config>,
    pub scheduler: GuildScheduler,
    pub player_manager: Arc<PlayerManager>,
}

#[async_trait]
pub trait GuildEventHandler: Send + Sync {
    // Serenity Events
    async fn new_message(&self, message: Message);

    async fn deleted_message(&self, channel: ChannelId, message: MessageId);

    async fn resume(&self, time: Instant, event: ResumedEvent);

    async fn voice_update(&self, old_voice_state: Option<VoiceState>, new_voice_state: VoiceState);

    // Lavalink Events
    async fn lavalink(&self, event: LavalinkEvent, client: LavalinkClient);

    // Reciprocity Events
    async fn main_message_error(&self, error: MessageError);

    async fn player_status_changed(&self);

    async fn main_message_emote_check(&self);

    async fn main_message_event(&self, event: EmoteAction, user: UserId);
}
