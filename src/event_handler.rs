use serenity::async_trait;

use log::{debug, error, info, warn};
use serenity::client::EventHandler as SerenityEventHandler;
use serenity::model::prelude::{ChannelId, GuildId, Message, MessageId, ResumedEvent, VoiceState};
use serenity::prelude::Context;
use smol::channel;
use smol::channel::{Receiver, SendError, Sender};
use smol::lock::Mutex;
use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::path::Iter;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use arraydeque::ArrayDeque;

pub const EVENT_STREAM_LIMIT: usize = 200;
pub const CACHE_SIZE: usize = 100;

type Cache = Arc<Mutex<ArrayDeque<[Event; CACHE_SIZE]>>>;

#[derive(Clone)]
pub struct EventHandler {
    cache: Arc<HashMap<GuildId, (Cache, Sender<Event>)>>,
}

#[derive(Error, Debug)]
pub enum EventHandlerError {
    #[error("Could not find {0:?} in Map")]
    NoGuild(GuildId),
    #[error("Could not send Event")]
    SendError(Box<SendError<Event>>),
}

impl EventHandlerError {
    pub fn is_send_error(&self) -> bool {
        matches!(self, EventHandlerError::SendError(_))
    }
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
    pub fn new(guilds: Vec<GuildId>) -> (EventHandler, Vec<(GuildId, Receiver<Event>)>) {
        let mut map = HashMap::new();
        let mut receiver = Vec::new();

        for guild in guilds {
            let cache = Arc::new(Mutex::new(ArrayDeque::new()));
            let (send, rec) = channel::bounded(EVENT_STREAM_LIMIT);

            map.borrow_mut().insert(guild, (cache, send));
            receiver.push((guild, rec));
        }

        (
            EventHandler {
                cache: Arc::new(map),
            },
            receiver,
        )
    }

    async fn process(&self, guild: GuildId, event: Event) -> Result<(), EventHandlerError> {
        match self.cache.get(&guild) {
            None => Err(EventHandlerError::NoGuild(guild)),
            Some((cache, sender)) => {
                if event.is_voice_update() || event.is_resume() {
                    sender
                        .send(event)
                        .await
                        .map_err(Box::new)
                        .map_err(EventHandlerError::SendError)
                } else {
                    {
                        let mut cache = cache.lock().await;
                        if cache.iter().any(|i| i == &event) {
                            return Ok(());
                        }
                        if cache.is_full(){
                            cache.pop_back().expect("Cache is empty");
                        }
                        cache.push_front(event.clone()).expect("Cache is full");
                    }
                    sender
                        .send(event)
                        .await
                        .map_err(Box::new)
                        .map_err(EventHandlerError::SendError)
                }
            }
        }
    }

    fn handle_result(result: Result<(), EventHandlerError>) {
        if let Err(e) = result {
            if e.is_send_error() {
                error!("{:?}", e);
                panic!(e);
            } else {
                debug!("{:?}", e);
            }
        }
    }
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

        for guild in self.cache.keys() {
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
