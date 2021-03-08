use arraydeque::ArrayDeque;
use log::{debug, error};
use serenity::async_trait;
use serenity::client::EventHandler as SerenityEventHandler;
use serenity::model::prelude::{ChannelId, GuildId, Message, MessageId, ResumedEvent, VoiceState};
use serenity::prelude::Context;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

pub const CACHE_SIZE: usize = 100;

type Cache = Arc<Mutex<ArrayDeque<[Event; CACHE_SIZE]>>>;
type EventHandlerGuild = (Cache, Arc<dyn GuildEventHandler>);

#[derive(Clone)]
pub struct EventHandler {
    cache: Arc<RwLock<HashMap<GuildId, EventHandlerGuild>>>,
}

#[derive(Error, Debug)]
pub enum EventHandlerError {
    #[error("Could not find {0:?} in Map")]
    NoGuild(GuildId),
}

#[derive(Clone)]
pub enum Event {
    NewMessage(Message),
    DeleteMessage(ChannelId, MessageId),
    Resume(Instant, ResumedEvent),
    VoiceUpdate(VoiceState),
}

impl Event {
    pub fn is_voice_update(&self) -> bool {
        matches!(self, Event::VoiceUpdate(_))
    }

    pub fn is_resume(&self) -> bool {
        matches!(self, Event::Resume(_, _))
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Event::NewMessage(m1), Event::NewMessage(m2)) => {
                m1.id == m2.id && m1.channel_id == m2.channel_id
            }
            (Event::DeleteMessage(c1, m1), Event::DeleteMessage(c2, m2)) => m1 == m2 && c1 == c2,
            (_, _) => false,
        }
    }
}

impl EventHandler {
    pub fn new() -> EventHandler {
        EventHandler {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_guild(&self, guild: GuildId, event_handler: Arc<dyn GuildEventHandler>) {
        let cache = Arc::new(Mutex::new(ArrayDeque::new()));
        self.cache
            .write()
            .await
            .insert(guild, (cache, event_handler));
    }

    async fn process(&self, guild: GuildId, event: Event) -> Result<(), EventHandlerError> {
        match self.cache.read().await.get(&guild) {
            None => Err(EventHandlerError::NoGuild(guild)),
            Some((cache, handler)) => {
                if !(event.is_voice_update() || event.is_resume()) {
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
                handler.run(event).await;
                Ok(())
            }
        }
    }

    fn handle_result(result: Result<(), EventHandlerError>) {
        if let Err(e) = result {
            debug!("{:?}", e);
        }
    }
}

#[async_trait]
pub trait GuildEventHandler: Send + Sync {
    async fn run(&self, event: Event);
}

#[async_trait]
impl SerenityEventHandler for EventHandler {
    async fn message(&self, _ctx: Context, new_message: Message) {
        let guild_id = match new_message.guild_id {
            Some(guild_id) => guild_id,
            None => return,
        };
        let event = Event::NewMessage(new_message);

        EventHandler::handle_result(self.process(guild_id, event).await);
    }

    async fn message_delete(
        &self,
        _ctx: Context,
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

    async fn resume(&self, _: Context, r: ResumedEvent) {
        //To every Guild
        let now = Instant::now();
        let event = Event::Resume(now, r);

        for guild in self.cache.read().await.keys() {
            EventHandler::handle_result(self.process(*guild, event.clone()).await);
        }
    }

    async fn voice_state_update(&self, _ctx: Context, guild_id: Option<GuildId>, v: VoiceState) {
        let guild_id = match guild_id {
            Some(guild_id) => guild_id,
            None => return,
        };
        let event = Event::VoiceUpdate(v);

        EventHandler::handle_result(self.process(guild_id, event).await);
    }
}
