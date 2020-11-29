#![allow(dead_code)]

use async_std::sync::Arc;
use serenity::async_trait;
use serenity::model::channel::{Message, Reaction, ReactionType};
use serenity::model::id::{ChannelId, MessageId, UserId};
use serenity::prelude::*;
use std::cmp::Eq;
use std::collections::{HashMap, HashSet, VecDeque};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, Instant};
use std::collections::hash_map::Entry;

const CACHE_DURATION: Duration = Duration::from_millis(900);

#[derive(Clone)]
struct Cache(Arc<Mutex<VecDeque<CachedEvent>>>);
impl Cache {
    pub fn new() -> Self {
        Cache(Arc::new(Mutex::new(VecDeque::new())))
    }

    pub async fn try_add(&self, event: Event) -> Result<(), Event> {
        let mut cache = self.0.lock().await;
        if self.contains(&event, &mut *cache) {
            return Err(event);
        }

        cache.push_back(CachedEvent::new(event));
        Ok(())
    }

    fn contains(&self, event: &Event, cache: &mut VecDeque<CachedEvent>) -> bool {
        self.clear_elapsed(cache);
        cache.iter().rev().any(|e| e.event == *event)
    }

    fn clear_elapsed(&self, cache: &mut VecDeque<CachedEvent>) {
        loop {
            //Check front
            match cache.front() {
                //If empty: return
                None => return,
                //Else check if front is already elapsed
                Some(c_event) => {
                    //It is elapsed: pop and try for the next item in the queue
                    if c_event.elapsed() {
                        cache.pop_front();
                    } else {
                        //Else return because no other event can be elapsed
                        return;
                    }
                }
            }
        }
    }
}

struct CachedEvent {
    event: Event,
    now: Instant,
}

impl CachedEvent {
    fn new(event: Event) -> Self {
        CachedEvent {
            event,
            now: Instant::now(),
        }
    }

    fn elapsed(&self) -> bool {
        self.now.elapsed() >= CACHE_DURATION
    }
}

#[derive(Eq, PartialEq, Clone)]
pub enum Event {
    NewMessage(MessageId),
    DeletedMessage(MessageId),
    AddedReaction(MessageId, UserId, ReactionType),
    RemovedReaction(MessageId, UserId, ReactionType),
}

type EventHandlerConfigType = Arc<
    RwLock<
        HashMap<
            ChannelId,
            (
                Arc<dyn ReciprocityGuildEventHandler + Send + Sync>,
                Arc<RwLock<HashSet<MessageId>>>,
            ),
        >,
    >,
>;

type Error = ReciprocityEventHandlerError;
pub enum ReciprocityEventHandlerError{
    ChannelDoesNotExist,
    ChannelAlreadyExists,
    MessageDoesNotExist,
    MessageAlreadyExists,
}

#[derive(Clone)]
pub struct ReciprocityEventHandler {
    channels: EventHandlerConfigType,
    bot_ids: HashSet<UserId>,
    cache: Cache,
}

impl ReciprocityEventHandler {
    pub fn new(bot_ids: HashSet<UserId>) -> ReciprocityEventHandler {
        ReciprocityEventHandler {
            channels: Arc::new(RwLock::new(HashMap::new())),
            bot_ids,
            cache: Cache::new(),
        }
    }

    async fn handle_event(
        &self,
        handler: Arc<dyn ReciprocityGuildEventHandler + Send + Sync>,
        event: Event,
    ) {
        // Check if this Event was already handled
        if self.cache.try_add(event.clone()).await.is_err() {
            return;
        }

        // Handle Event in Guild
        handler.handle(event).await;
    }

    pub async fn process_event_new_message(&self, msg: Message) {
        let handler = match self.channels.read().await.get(&msg.channel_id) {
            None => return,
            Some((handler, _)) => handler.clone(),
        };

        self.handle_event(handler, Event::NewMessage(msg.id)).await;
    }

    pub async fn process_event_message_deleted(&self, channel: ChannelId, msg: MessageId) {
        let (handler, messages) = match self.channels.read().await.get(&channel) {
            None => return,
            Some(info) => info.clone(),
        };

        //If message is not relevant: Return
        if !messages.read().await.contains(&msg) {
            return;
        }

        self.handle_event(handler, Event::DeletedMessage(msg)).await;
    }

    pub async fn process_event_reaction_added(&self, reaction: Reaction) {
        let user = reaction.user_id.expect("No User in Reaction");

        let (handler, messages) = match self.channels.read().await.get(&reaction.channel_id) {
            None => return,
            Some(info) => info.clone(),
        };

        //If message is not relevant or if bot added the reaction: Return
        if !messages.read().await.contains(&reaction.message_id) || self.bot_ids.contains(&user) {
            return;
        }

        self.handle_event(
            handler,
            Event::AddedReaction(reaction.message_id, user, reaction.emoji),
        )
            .await;
    }

    pub async fn process_event_reaction_removed(&self, reaction: Reaction) {
        let user = reaction.user_id.expect("No User in Reaction");

        let (handler, messages) = match self.channels.read().await.get(&reaction.channel_id) {
            None => return,
            Some(info) => info.clone(),
        };

        //If message is not relevant or if reaction was not from a bot: Return
        if !messages.read().await.contains(&reaction.message_id) || !self.bot_ids.contains(&user) {
            return;
        }

        self.handle_event(
            handler,
            Event::RemovedReaction(reaction.message_id, user, reaction.emoji),
        )
            .await;
    }

    pub async fn add_guild(&self, channel: ChannelId, handler: Arc<dyn ReciprocityGuildEventHandler + Send + Sync>) -> Result<(), Error>{
        match self.channels.write().await.entry(channel) {
            Entry::Occupied(_) => Err(Error::ChannelAlreadyExists),
            Entry::Vacant(entry) => {
                entry.insert((handler, Arc::new(RwLock::new(HashSet::new()))));
                Ok(())
            },
        }
    }

    pub async fn remove_guild(&self, channel: ChannelId) -> Result<(), Error>{
        match self.channels.write().await.entry(channel) {
            Entry::Occupied(entry) => {
                entry.remove();
                Ok(())
            }
            Entry::Vacant(_) => Err(Error::ChannelDoesNotExist)
        }
    }

    pub async fn remove_message(&self, channel: ChannelId, message: MessageId) -> Result<(), Error>{
        let messages = match self.channels.read().await.get(&channel) {
            None => return Err(Error::ChannelDoesNotExist),
            Some((_, messages)) => messages.clone(),
        };

        let mut messages = messages.write().await;
        match messages.remove(&message) {
            true => Ok(()),
            false => Err(Error::MessageDoesNotExist)
        }
    }

    pub async fn remove_messages(&self, channel: ChannelId, remove_messages: HashSet<MessageId>) -> Result<(), Error> {
        let messages = match self.channels.read().await.get(&channel) {
            None => return Err(Error::ChannelDoesNotExist),
            Some((_, messages)) => messages.clone(),
        };

        let mut messages = messages.write().await;
        for m in remove_messages.iter(){
            messages.remove(m);
        }

        Ok(())
    }

    pub async fn add_message(&self, channel: ChannelId, message: MessageId) -> Result<(), Error>{
        let messages = match self.channels.read().await.get(&channel) {
            None => return Err(Error::ChannelDoesNotExist),
            Some((_, messages)) => messages.clone(),
        };

        let mut messages = messages.write().await;
        match messages.insert(message){
            true => Ok(()),
            false => Err(Error::MessageAlreadyExists)
        }
    }

    pub async fn add_messages(&self, channel: ChannelId, add_messages: HashSet<MessageId>) -> Result<(), Error>{
        let messages = match self.channels.read().await.get(&channel) {
            None => return Err(Error::ChannelDoesNotExist),
            Some((_, messages)) => messages.clone(),
        };

        let mut messages = messages.write().await;
        for m in add_messages.iter(){
            messages.insert(*m);
        }

        Ok(())
    }

    async fn clear_messages(&self, channel: ChannelId) -> Result<(), Error>{
        let messages = match self.channels.read().await.get(&channel) {
            None => return Err(Error::ChannelDoesNotExist),
            Some((_, messages)) => messages.clone(),
        };

        let mut messages = messages.write().await;
        messages.clear();
        Ok(())
    }

    async fn change_channel(&self, old_channel: ChannelId, new_channel: ChannelId) -> Result<(), Error>{
        let mut channels = self.channels.write().await;
        match channels.contains_key(&new_channel) {
            true => return Err(Error::ChannelAlreadyExists),
            false => {
                let value = match channels.entry(old_channel) {
                    Entry::Vacant(_) => return Err(Error::ChannelDoesNotExist),
                    Entry::Occupied(entry) => entry.remove(),
                };
                channels.insert(new_channel, value);
            },
        };

        Ok(())
    }
}

#[async_trait]
impl EventHandler for ReciprocityEventHandler {
    async fn message(&self, _: Context, msg: Message) {
        self.process_event_new_message(msg).await;
    }

    async fn message_delete(&self, _: Context, channel: ChannelId, msg: MessageId) {
        self.process_event_message_deleted(channel, msg).await;
    }

    async fn reaction_add(&self, _: Context, reaction: Reaction) {
        self.process_event_reaction_added(reaction).await;
    }

    async fn reaction_remove(&self, _: Context, reaction: Reaction) {
        self.process_event_reaction_removed(reaction).await;
    }
}

#[async_trait]
pub trait ReciprocityGuildEventHandler {
    async fn handle(&self, event: Event);
}
