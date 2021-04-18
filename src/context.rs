use crate::bots::BotMap;
use crate::config::Config;
use crate::event_handler::EventHandler;
use crate::guild::message_manager::{EmoteAction, MainMessage};
use crate::guild::player_manager::PlayerManager;
use crate::guild::scheduler::GuildScheduler;
use crate::lavalink_handler::LavalinkEvent;
use lavalink_rs::LavalinkClient;
use serenity::async_trait;
use serenity::model::prelude::{
    ChannelId, GuildId, Message, MessageId, ResumedEvent, UserId, VoiceState,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

pub type MainMessageData = (MainMessage, JoinHandle<()>);

#[derive(Clone)]
pub struct Context {
    pub id: GuildId,
    pub channel: ChannelId,
    pub bots: Arc<BotMap>,
    pub event_handler: EventHandler,
    pub config: Arc<Config>,
    pub scheduler: GuildScheduler,
    pub player_manager: Arc<PlayerManager>,
    pub search_messages: Arc<RwLock<HashMap<UserId, MessageId>>>,
    pub main_message: Arc<RwLock<Option<MainMessageData>>>,
}

#[async_trait]
pub trait GuildEventHandler: Send + Sync {
    // Serenity Events
    /// When a new Message was received
    async fn new_message(&self, message: Message);

    /// When a Message was deleted
    async fn deleted_message(&self, channel: ChannelId, message: MessageId);

    /// When all reactions of a message were deleted
    async fn bulk_reaction_delete(&self, channel: ChannelId, message: MessageId);

    /// When DiscordAPI disconnected but resumed
    async fn resume(&self, time: Instant, event: ResumedEvent);

    /// When a VoiceState Changed
    async fn voice_update(
        &self,
        old_voice_state: Option<VoiceState>,
        new_voice_state: VoiceState,
        bot: UserId,
    );

    // Lavalink Events
    /// When any Lavalink Event was received
    async fn lavalink(&self, event: LavalinkEvent, client: LavalinkClient);

    // Reciprocity Events
    /// When the Main Message produced an event
    async fn main_message_event(&self, event: EmoteAction, user: UserId);

    /// When the Main Message might be missing
    async fn check_main_message(&self);
}
