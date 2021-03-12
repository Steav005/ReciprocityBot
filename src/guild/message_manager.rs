use crate::guild::player_manager::PlayerManager;
use crate::guild::scheduler::GuildScheduler;
use crate::guild::ReciprocityGuild;
use crate::task_handle::{AddMessageReactionTask, DeleteMessageReactionTask, DeleteMessageTask};
use crate::BotList;
use futures::FutureExt;
use futures::StreamExt;
use lavalink_rs::model::Track;
use serde_json::Value;
use serenity::client::bridge::gateway::ShardMessenger;
use serenity::client::Cache;
use serenity::collector::ReactionAction;
use serenity::model::guild::Target::Emoji;
use serenity::model::prelude::{
    ChannelId, CurrentUser, Message, MessageId, Reaction, ReactionType, User, UserId,
};
use serenity::prelude::SerenityError;
use serenity::utils::MessageBuilder;
use serenity::{async_trait, CacheAndHttp};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::future::Future;
use std::ops::{Deref, Index};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

const DELETE_MESSAGE_DELAY: Duration = Duration::from_millis(500);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
const SEARCH_TITLE_LIMIT: usize = 40;

pub type SearchMessageMap = Arc<RwLock<HashMap<UserId, (MessageId, JoinHandle<()>)>>>;
pub type MainMessageMutex = Arc<Mutex<Option<(MessageId, JoinHandle<()>)>>>;

#[async_trait]
pub trait MessageManager: Send + Sync {
    ///Important Init call<br>
    /// TODO überlegen was hier überhaupt gemacht wird
    fn init_message_manager(&mut self);

    /// Handles incoming message
    /// - Check if message is not from our bot
    /// - Check if channel fits<br>
    /// - Sends Search Request Event to MessageEventHandler
    /// - Deletes Message
    async fn handle_message(&'static self, message: Message);

    /// Creates Search Message for specific User by specific bot with specific Tracks
    async fn create_search(&'static self, requester: UserId, bot: UserId, tracks: Vec<Track>);

    /// Gets any deleted Message
    /// - Checks if message is search message
    /// - TODO Checks if message is main message
    /// - Removes message and aborts message handle
    async fn deleted_bot_message(&'static self, message: MessageId);
}

#[async_trait]
impl MessageManager for ReciprocityGuild {
    fn init_message_manager(&mut self) {
        let bots: Vec<_> = self
            .bots
            .values()
            .map(|(cache_http, _)| cache_http.clone())
            .collect();

        tokio::spawn(async move {});
    }

    async fn handle_message(&'static self, message: Message) {
        //Exit if message is from our bot
        if self.bots.contains_key(&message.author.id) {
            return;
        }
        let delete_task = DeleteMessageTask {
            channel: message.channel_id,
            message: message.id,
        };

        tokio::spawn(self.run(MessageManagerEvent::SearchInput(
            message.author.id,
            message.content,
        )));

        tokio::time::sleep(DELETE_MESSAGE_DELAY).await;
        self.process(delete_task).await.ok();
    }

    async fn create_search(&'static self, requester: UserId, bot: UserId, tracks: Vec<Track>) {
        if let Some((cache_http, _)) = self.bots.get(&bot) {
            if let Some(shard) = self.event_handler.get_shard_sender(self.id, bot).await {
                let search = SearchMessage::start(
                    self.channel,
                    requester,
                    (cache_http.clone(), shard),
                    tracks,
                    self.clone(),
                    self.clone(),
                );

                if let Ok(search) = search.await {
                    if let Some((_, old_search)) =
                        self.search_messages.write().await.insert(requester, search)
                    {
                        old_search.abort();
                    };
                }
            }
        }
    }

    async fn deleted_bot_message(&'static self, message: MessageId) {
        let lock = self.search_messages.read().await;
        if let Some(key) = lock
            .iter()
            .find(|(_, (id, _))| message.eq(id))
            .map(|(user, _)| user)
            .cloned()
        {
            drop(lock);
            let mut lock = self.search_messages.write().await;
            if let Some((id, _)) = lock.get(&key) {
                if message.eq(id) {
                    if let Some((_, handle)) = lock.remove(&key) {
                        drop(lock);
                        handle.abort();
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum MessageManagerEvent {
    PlayPauseClick(UserId),
    PrevClick(UserId),
    NextClick(UserId),
    LoopAllClick(UserId),
    LoopOneClick(UserId),
    JoinClick(UserId),
    LeaveClick(UserId),
    SearchInput(UserId, String),
    TrackSelect(UserId, Track),
    ClearQueueClick(UserId),
}

#[async_trait]
pub trait MessageEventHandler: Send + Sync {
    async fn run(&self, event: MessageManagerEvent);
}

#[derive(Debug, Clone)]
pub struct MainMessage<S, P>
where
    S: GuildScheduler + Clone + 'static,
    P: PlayerManager + 'static,
{
    id: MessageId,
    message: Message,
    channel: ChannelId,
    scheduler: S,
    player_manager: P,
}

#[derive(Error, Debug)]
pub enum MainMessageError {
    #[error("{0:?}")]
    Serenity(SerenityError),
    #[error("Could not find Bot for Guild")]
    NoBot(),
}

impl<S, P> MainMessage<S, P>
where
    S: GuildScheduler + Clone + 'static,
    P: PlayerManager + 'static,
{
    async fn start(
        channel: ChannelId,
        bot: (Arc<CacheAndHttp>, ShardMessenger),
        scheduler: S,
        player_manager: P,
        event_handler: impl MessageEventHandler + Clone + 'static,
    ) -> Result<(MessageId, JoinHandle<()>), MainMessageError> {
        let message = bot
            .0
            .http
            .send_message(
                channel.0,
                &Self::generate_content(&player_manager, bot.0.cache.clone()).await,
            )
            .await
            .map_err(MainMessageError::Serenity)?;

        let main = MainMessage {
            id: message.id,
            message: message.clone(),
            channel,
            scheduler: scheduler.clone(),
            player_manager,
        };

        Ok((
            message.id,
            tokio::spawn(main.run(bot.1.clone(), event_handler)),
        ))
    }

    async fn run(
        self,
        shard: ShardMessenger,
        event_handler: impl MessageEventHandler + Clone + 'static,
    ) {
        let mut collector = self
            .message
            .clone()
            .await_reactions(&shard)
            .added(true)
            .removed(true)
            .await;

        while let Some(reaction) = collector.next().await {
            match reaction.as_ref() {
                ReactionAction::Added(reaction) => {
                    todo!("Filter Bots out");
                    let reaction = reaction.deref().clone();
                    tokio::spawn(Self::delete_reaction(
                        reaction.clone(),
                        self.scheduler.clone(),
                    ));
                    if let Some(user) = reaction.user_id {
                        if let Ok(action) = EmoteAction::try_from(&reaction) {
                            let event = match action {
                                EmoteAction::PlayPause() => {
                                    MessageManagerEvent::PlayPauseClick(user)
                                }
                                EmoteAction::Next() => MessageManagerEvent::NextClick(user),
                                EmoteAction::Prev() => MessageManagerEvent::PrevClick(user),
                                EmoteAction::Join() => MessageManagerEvent::JoinClick(user),
                                EmoteAction::Leave() => MessageManagerEvent::LeaveClick(user),
                                EmoteAction::LoopOne() => MessageManagerEvent::LoopOneClick(user),
                                EmoteAction::LoopAll() => MessageManagerEvent::LoopAllClick(user),
                                EmoteAction::Nothing() => {
                                    MessageManagerEvent::ClearQueueClick(user)
                                }
                                _ => continue,
                            };
                            tokio::spawn(Self::send_event(event, event_handler.clone()));
                        }
                    }
                }
                ReactionAction::Removed(_) => {
                    todo!()
                }
            }
        }
    }

    async fn send_event(event: MessageManagerEvent, event_handler: impl MessageEventHandler) {
        event_handler.run(event).await;
    }

    async fn delete_reaction(reaction: Reaction, scheduler: impl GuildScheduler) {
        if let Some(user) = reaction.user_id {
            scheduler
                .process(DeleteMessageReactionTask {
                    channel: reaction.channel_id,
                    message: reaction.message_id,
                    user,
                    reaction: reaction.emoji,
                })
                .await
                .ok();
        }
    }

    async fn generate_content(player_manager: &P, cache: Arc<Cache>) -> Value {
        let mut content = String::default();
        for (channel, state) in player_manager.get_all_player_states().await {
            if let Some(user) = cache.user(state.bot).await {
                let mut intermediary_content = String::default();
                if let Some((dur, track)) = &state.current {}
            }
        }

        todo!()
    }
}

impl<S, P> Drop for MainMessage<S, P>
where
    S: GuildScheduler + Clone + 'static,
    P: PlayerManager + 'static,
{
    fn drop(&mut self) {
        delete_message(self.id, self.channel, self.scheduler.clone())
    }
}

#[derive(Debug)]
struct SearchMessage<S>
where
    S: GuildScheduler + Clone + 'static,
{
    id: MessageId,
    message: Message,
    requester: UserId,
    channel: ChannelId,
    tracks: Arc<Vec<Track>>,
    scheduler: S,
}

impl<S> SearchMessage<S>
where
    S: GuildScheduler + Clone + 'static,
{
    async fn start(
        channel: ChannelId,
        requester: UserId,
        bot: (Arc<CacheAndHttp>, ShardMessenger),
        tracks: Vec<Track>,
        scheduler: S,
        event_handler: impl MessageEventHandler + 'static,
    ) -> Result<(MessageId, JoinHandle<()>), SerenityError> {
        //Create Search Message
        let message = bot
            .0
            .http
            .send_message(channel.0, &Self::generate_content(&tracks))
            .await?;
        let message_id = message.id;

        let search = SearchMessage {
            id: message_id,
            message,
            channel,
            tracks: Arc::new(tracks),
            requester,
            scheduler,
        };

        Ok((
            search.message.id,
            tokio::spawn(search.run(bot.1, event_handler)),
        ))
    }

    async fn run(self, shard: ShardMessenger, event_handler: impl MessageEventHandler + 'static) {
        //Build emotes, that we are using with this message
        let emotes: Arc<Vec<_>> = Arc::new(
            (1..self.tracks.len())
                .take(10)
                .map(EmoteAction::Number)
                .chain(vec![EmoteAction::Delete()])
                .collect(),
        );
        let emotes_1 = emotes.clone();

        //Build Collector
        let tracks = self.tracks.clone();
        let collector = self
            .message
            .clone()
            .await_reaction(&shard)
            .timeout(SEARCH_TIMEOUT)
            .author_id(self.requester)
            .removed(false)
            .added(true)
            .filter(move |reaction| {
                emotes
                    .iter()
                    .any(|e| reaction.emoji.unicode_eq(e.unicode()))
            })
            //Map into EmoteAction
            .map(|r| {
                if let Some(r) = r {
                    EmoteAction::try_from(r.as_inner_ref().deref()).ok()
                } else {
                    None
                }
            })
            //Map into Track
            .map(move |e| {
                if let Some(EmoteAction::Number(i)) = e {
                    return tracks.get(i - 1).cloned();
                }
                None
            });

        //Add all emotes to the message
        tokio::spawn(Self::add_emotes(
            self.id,
            self.channel,
            emotes_1,
            self.scheduler.clone(),
        ));

        //Await reaction and handle it
        if let Some(track) = collector.await {
            let requester = self.requester;
            let event_handler = Box::pin(event_handler);

            //Call EventHandler with selected Track
            tokio::spawn(async move {
                event_handler
                    .run(MessageManagerEvent::TrackSelect(requester, track))
                    .await;
            });
        }
    }

    ///Adds a list of emotes to a message
    async fn add_emotes(
        message: MessageId,
        channel: ChannelId,
        emotes: Arc<Vec<EmoteAction>>,
        scheduler: impl GuildScheduler,
    ) {
        for task in emotes.iter().map(|emote| AddMessageReactionTask {
            channel,
            message,
            reaction: emote.reaction(),
        }) {
            //Add single emote and wait for completion
            scheduler.process(task).await.ok();
        }
    }

    fn generate_content(tracks: &[Track]) -> Value {
        let content: String = tracks
            .iter()
            .enumerate()
            .map(|(i, track)| {
                format!(
                    "{}: {}\n\r",
                    i + 1,
                    track
                        .clone()
                        .info
                        .map_or("Missing Name".to_string(), |info| info
                            .title
                            .index(..SEARCH_TITLE_LIMIT)
                            .to_string())
                )
            })
            .collect();

        let content = MessageBuilder::new()
            .push_codeblock(content, Some("css"))
            .build();
        Value::String(content)
    }
}

///If dropped, try deleting the Message
impl<S> Drop for SearchMessage<S>
where
    S: GuildScheduler + Clone + 'static,
{
    fn drop(&mut self) {
        delete_message(self.id, self.channel, self.scheduler.clone());
    }
}

#[derive(Clone, Copy, Debug)]
pub enum EmoteAction {
    Number(usize),
    PlayPause(),
    Next(),
    Prev(),
    Join(),
    Leave(),
    Delete(),
    LoopOne(),
    LoopAll(),
    Nothing(),
}

impl EmoteAction {
    const NUMBERS: [&'static str; 11] = ["0️⃣", "1️⃣", "2️⃣", "3️⃣", "4️⃣", "5️⃣", "6️⃣", "7️⃣", "8️⃣", "9️⃣", "🔟"];
    const PLAY_PAUSE: &'static str = "⏯";
    const NEXT: &'static str = "⏭";
    const PREV: &'static str = "⏮";
    const JOIN: &'static str = "📥";
    const LEAVE: &'static str = "📤";
    const DELETE: &'static str = "❌";
    const LOOP_ONE: &'static str = "🔂";
    const LOOP_ALL: &'static str = "🔁";
    const NOTHING: &'static str = "無";

    pub fn unicode(&self) -> &str {
        match self {
            EmoteAction::Number(number) => Self::NUMBERS.get(*number).unwrap_or(&""),
            EmoteAction::PlayPause() => Self::PLAY_PAUSE,
            EmoteAction::Next() => Self::NEXT,
            EmoteAction::Prev() => Self::PREV,
            EmoteAction::Join() => Self::JOIN,
            EmoteAction::Leave() => Self::LEAVE,
            EmoteAction::Delete() => Self::DELETE,
            EmoteAction::LoopOne() => Self::LOOP_ONE,
            EmoteAction::LoopAll() => Self::LOOP_ALL,
            EmoteAction::Nothing() => Self::NOTHING,
        }
    }

    pub fn reaction(&self) -> ReactionType {
        ReactionType::Unicode(self.unicode().to_string())
    }
}

impl TryFrom<&Reaction> for EmoteAction {
    type Error = ();

    fn try_from(value: &Reaction) -> Result<Self, Self::Error> {
        if let ReactionType::Unicode(str) = value.emoji.borrow() {
            return EmoteAction::try_from(str.as_str());
        }
        Err(())
    }
}

impl TryFrom<&str> for EmoteAction {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            Self::PLAY_PAUSE => Ok(Self::PlayPause()),
            Self::NEXT => Ok(Self::Next()),
            Self::PREV => Ok(Self::Prev()),
            Self::JOIN => Ok(Self::Join()),
            Self::LEAVE => Ok(Self::Leave()),
            Self::DELETE => Ok(Self::Delete()),
            Self::LOOP_ONE => Ok(Self::LoopOne()),
            Self::LOOP_ALL => Ok(Self::LoopAll()),
            Self::NOTHING => Ok(Self::Nothing()),
            _ => {
                if let Some(i) = Self::NUMBERS.iter().position(|n| value.eq(*n)) {
                    Ok(Self::Number(i))
                } else {
                    Err(())
                }
            }
        }
    }
}

fn delete_message<S>(message: MessageId, channel: ChannelId, scheduler: S)
where
    S: GuildScheduler + 'static,
{
    tokio::spawn(async move {
        scheduler
            .process(DeleteMessageTask { channel, message })
            .await
    });
}
