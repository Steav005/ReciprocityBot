use crate::context::GuildEventHandler;
use arraydeque::ArrayDeque;
use log::{debug, error, info};
use serenity::async_trait;
use serenity::client::bridge::gateway::ShardMessenger;
use serenity::client::EventHandler as SerenityEventHandler;
use serenity::model::prelude::{
    ChannelId, GuildId, Message, MessageId, ResumedEvent, UserId, VoiceState,
};
use serenity::prelude::Context;
use std::collections::HashMap;
use std::ops::BitAnd;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

pub const CACHE_SIZE: usize = 100;
pub const VOICE_STATE_MAX_DIFF: Duration = Duration::from_millis(500);

type Cache = Arc<Mutex<ArrayDeque<[Event; CACHE_SIZE]>>>;
type EventHandlerGuild = (Cache, Arc<dyn GuildEventHandler>);
type GuildShardSenderMap = RwLock<HashMap<UserId, (u64, ShardMessenger)>>;

#[derive(Clone)]
pub struct EventHandler {
    shard_sender: Arc<RwLock<HashMap<GuildId, GuildShardSenderMap>>>,
    cache: Arc<RwLock<HashMap<GuildId, EventHandlerGuild>>>,
}

#[derive(Error, Debug)]
pub enum EventHandlerError {
    #[error("Could not find {0:?} in Map")]
    NoGuild(GuildId),
}

#[derive(Clone, Debug)]
pub enum Event {
    NewMessage(Message),
    DeleteMessage(ChannelId, MessageId),
    Resume(Instant, ResumedEvent),
    ///Old VoiceState / New VoiceState / Detecting Bot
    VoiceUpdate(Option<VoiceState>, VoiceState, UserId, Instant),
    BulkReactionDelete(ChannelId, MessageId),
    CacheReady(UserId),
}

impl Event {
    pub fn is_cached(&self) -> bool {
        matches!(self, Event::NewMessage(_))
            || matches!(self, Event::DeleteMessage(_, _))
            || matches!(self, Event::VoiceUpdate(_, _, _, _))
    }
}

pub fn duration_diff(dur1: Duration, dur2: Duration) -> Duration {
    if dur1 > dur2 {
        dur1 - dur2
    } else {
        dur2 - dur1
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Event::NewMessage(m1), Event::NewMessage(m2)) => {
                m1.id == m2.id && m1.channel_id == m2.channel_id
            }
            (Event::DeleteMessage(c1, m1), Event::DeleteMessage(c2, m2)) => m1 == m2 && c1 == c2,
            (Event::VoiceUpdate(o1, n1, _, i1), Event::VoiceUpdate(o2, n2, _, i2)) => {
                let diff = duration_diff(i1.elapsed(), i2.elapsed());
                if diff > VOICE_STATE_MAX_DIFF {
                    return false;
                }
                o1.is_some()
                    .eq(&o2.is_some())
                    .bitand(n1.session_id.eq(&n2.session_id))
                    .bitand(n1.user_id.eq(&n2.user_id))
                    .bitand(n1.channel_id.eq(&n2.channel_id))
            }
            (Event::BulkReactionDelete(c1, m1), Event::BulkReactionDelete(c2, m2)) => {
                m1 == m2 && c1 == c2
            }
            (_, _) => false,
        }
    }
}

impl EventHandler {
    pub fn new() -> EventHandler {
        EventHandler {
            shard_sender: Arc::new(RwLock::new(HashMap::new())),
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_guild(&self, guild: GuildId, event_handler: Arc<dyn GuildEventHandler>) {
        info!("Adding Guild to Event Handler: {:?}", guild);
        let cache = Arc::new(Mutex::new(ArrayDeque::new()));
        self.cache
            .write()
            .await
            .insert(guild, (cache, event_handler));
        self.shard_sender
            .write()
            .await
            .insert(guild, RwLock::new(HashMap::new()));
    }

    async fn process(&self, guild: GuildId, event: Event) -> Result<(), EventHandlerError> {
        let (event, handler) = match self.cache.read().await.get(&guild) {
            None => return Err(EventHandlerError::NoGuild(guild)),
            Some((cache, handler)) => {
                if event.is_cached() {
                    {
                        let mut cache = cache.lock().await;
                        if cache.iter().any(|i| i == &event) {
                            return Ok(());
                        }
                        if cache.is_full() {
                            cache.pop_back().expect("Cache is empty");
                        }
                        cache.push_front(event.clone()).expect("Cache is full");
                    }
                }
                (event, handler.clone())
            }
        };
        debug!("Got new Event: {:?}", &event);
        match event {
            Event::NewMessage(msg) => handler.new_message(msg).await,
            Event::DeleteMessage(ch, msg) => handler.deleted_message(ch, msg).await,
            Event::Resume(t, e) => handler.resume(t, e).await,
            Event::VoiceUpdate(ov, nv, bot, _) => handler.voice_update(ov, nv, bot).await,
            Event::BulkReactionDelete(ch, msg) => handler.bulk_reaction_delete(ch, msg).await,
            Event::CacheReady(bot) => handler.cache_ready(bot).await,
        }
        Ok(())
    }

    fn handle_result(result: Result<(), EventHandlerError>) {
        if let Err(e) = result {
            debug!("{:?}", e);
        }
    }

    pub async fn get_shard_sender(&self, guild: GuildId, bot: UserId) -> Option<ShardMessenger> {
        if let Some(guild) = self.shard_sender.read().await.get(&guild) {
            if let Some((_, shard)) = guild.read().await.get(&bot) {
                return Some(shard.clone());
            }
        }
        None
    }

    async fn replace_shard_sender(&self, ctx: Context, guild_id: GuildId) {
        if let Some(guild) = self.shard_sender.read().await.get(&guild_id) {
            let bot_id = ctx.cache.current_user_id().await;
            let read_lock = guild.read().await;
            if let Some((id, _)) = read_lock.get(&bot_id) {
                let id = *id;
                if id == ctx.shard_id {
                    return;
                }
            }
            drop(read_lock);
            info!(
                "Got new Shard: {:?}, {:?}, Bot: {:?}",
                ctx.shard_id, guild_id, bot_id
            );
            guild
                .write()
                .await
                .insert(bot_id, (ctx.shard_id, ctx.shard));
        }
    }
}

#[async_trait]
impl SerenityEventHandler for EventHandler {
    async fn cache_ready(&self, ctx: Context, guilds: Vec<GuildId>) {
        let event = Event::CacheReady(ctx.cache.current_user_id().await);
        for guild_id in guilds {
            self.replace_shard_sender(ctx.clone(), guild_id).await;
            let s = self.clone();
            let e = event.clone();
            tokio::spawn(async move { EventHandler::handle_result(s.process(guild_id, e).await) });
        }
    }

    async fn message(&self, ctx: Context, new_message: Message) {
        let guild_id = match new_message.guild_id {
            Some(guild_id) => guild_id,
            None => return,
        };
        self.replace_shard_sender(ctx, guild_id).await;

        let event = Event::NewMessage(new_message);
        EventHandler::handle_result(self.process(guild_id, event).await);
    }

    async fn message_delete(
        &self,
        _: Context,
        channel_id: ChannelId,
        deleted_message_id: MessageId,
        guild_id: Option<GuildId>,
    ) {
        let guild_id = match guild_id {
            Some(guild_id) => guild_id,
            None => return,
        };
        let event = Event::DeleteMessage(channel_id, deleted_message_id);

        EventHandler::handle_result(self.process(guild_id, event).await);
    }

    async fn reaction_remove_all(
        &self,
        ctx: Context,
        channel_id: ChannelId,
        message_id: MessageId,
    ) {
        let ch = match ctx.cache.channel(channel_id).await {
            Some(msg) => msg,
            None => return,
        };
        let guild_id = match ch.guild() {
            Some(guild_ch) => guild_ch.guild_id,
            None => return,
        };

        let event = Event::BulkReactionDelete(channel_id, message_id);
        EventHandler::handle_result(self.process(guild_id, event).await);
    }

    async fn resume(&self, _: Context, r: ResumedEvent) {
        //To every Guild
        let now = Instant::now();
        let event = Event::Resume(now, r);

        for guild in self.cache.read().await.keys() {
            EventHandler::handle_result(self.process(*guild, event.clone()).await);
        }
    }

    async fn voice_state_update(
        &self,
        ctx: Context,
        guild_id: Option<GuildId>,
        old: Option<VoiceState>,
        new: VoiceState,
    ) {
        let guild_id = match guild_id {
            Some(guild_id) => guild_id,
            None => return,
        };

        let now = Instant::now();

        //Send Event to Guild
        let event = Event::VoiceUpdate(old, new, ctx.cache.current_user_id().await, now);
        EventHandler::handle_result(self.process(guild_id, event).await);
    }
}
