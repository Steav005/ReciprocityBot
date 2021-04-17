use crate::bots::Bot;
use crate::context::{Context, GuildEventHandler};
use crate::guild::scheduler::GuildScheduler;
use crate::guild::ReciprocityGuild;
use crate::task_handle::{AddMessageReactionTask, DeleteMessageReactionTask, DeleteMessageTask};
use futures::FutureExt;
use lavalink_rs::model::Track;
use serde_json::Value;
use serenity::client::bridge::gateway::ShardMessenger;
use serenity::collector::ReactionAction;
use serenity::http::Http;
use serenity::model::prelude::{
    ChannelId, GuildId, Message, MessageId, Reaction, ReactionType, UserId,
};
use serenity::prelude::SerenityError;
use serenity::utils::MessageBuilder;
use std::borrow::Borrow;
use std::convert::{TryFrom, TryInto};
use std::ops::{Deref, Index};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use std::fmt::Write;
use std::convert::AsRef;

const DELETE_MESSAGE_DELAY: Duration = Duration::from_millis(500);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
const SEARCH_TITLE_LIMIT: usize = 40;
const MESSAGE_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

///Adds a list of emotes to a message
async fn add_emotes(
    message: MessageId,
    channel: ChannelId,
    emotes: Arc<Vec<EmoteAction>>,
    scheduler: GuildScheduler,
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

pub struct SearchMessage;

impl SearchMessage {
    pub async fn search(
        bot: Arc<Http>,
        tracks: Vec<Track>,
        requester: UserId,
        shard_messenger: impl AsRef<ShardMessenger>,
        context: Context,
    ) -> Result<Track, MessageError> {
        let message = bot
            .send_message(context.channel.0, &Self::content(&tracks))
            .await
            .map_err(MessageError::SerenityError)?;

        //Build emotes, that we are using with this message
        let emotes: Arc<Vec<_>> = Arc::new(
            (1..tracks.len())
                .take(10)
                .map(EmoteAction::Number)
                .chain(vec![EmoteAction::Delete()])
                .collect(),
        );
        let emotes_1 = emotes.clone();

        let filter =
            move |r: &Arc<Reaction>| emotes.iter().any(|e| r.emoji.unicode_eq(e.unicode()));
        let collector = message
            .await_reaction(&shard_messenger)
            .timeout(SEARCH_TIMEOUT)
            .author_id(requester.0)
            .removed(false)
            .added(true)
            .filter(filter)
            //Map into EmoteAction
            .map(|r| EmoteAction::try_from(r?.as_inner_ref().deref()).ok())
            //Map into Track
            .map(move |e| {
                if let Some(EmoteAction::Number(i)) = e {
                    return tracks.get(i - 1).cloned();
                }
                None
            });
        tokio::spawn(add_emotes(
            message.id,
            context.channel,
            emotes_1.clone(),
            context.scheduler.clone(),
        ));
        let track: Result<Track, MessageError> = collector.await.ok_or(MessageError::Timeout());

        context
            .scheduler
            .process_enqueue(DeleteMessageTask {
                channel: context.channel,
                message: message.id,
            })
            .await
            .ok();

        track
    }

    fn content(tracks: &[Track]) -> Value {
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

#[derive(Clone)]
pub struct MainMessage {
    lock: Arc<Mutex<()>>,
    message: Message,
    bot: Arc<Bot>,
    shard: ShardMessenger,
    context: Context,
}

impl MainMessage {
    pub async fn new(guild: ReciprocityGuild, context: Context) -> Result<Self, MessageError> {
        let bot = context
            .bots
            .get_any_guild_bot(&context.id)
            .await
            .ok_or(MessageError::NoBot(context.id))?;
        let shard = context
            .event_handler
            .get_shard_sender(context.id, bot.id())
            .await
            .ok_or_else(|| MessageError::NoShard(bot.id()))?;

        let message = bot
            .http()
            .send_message(context.channel.0, &Self::content(&context).await)
            .await
            .map_err(MessageError::SerenityError)?;
        let main_message = MainMessage {
            lock: Arc::new(Mutex::new(())),
            message,
            bot,
            shard,
            context,
        };
        tokio::spawn(main_message.clone().update());
        tokio::spawn(main_message.clone().run(guild));
        Ok(main_message)
    }

    pub async fn run(self, guild: ReciprocityGuild) {
        let cloned_guild = guild.clone();
        tokio::spawn(async move { cloned_guild.main_message_emote_check().await });

        let mut collector = self
            .message
            .await_reactions(&self.shard)
            .added(true)
            .removed(true)
            .await;

        while let Some(reaction) = collector.next().await {
            match reaction.deref() {
                ReactionAction::Added(reaction) => {
                    if let Some(user) = &reaction.user_id {
                        if self.context.bots.contains_id(user) {
                            continue;
                        }
                        // Pass on Event to Guild
                        if let Ok(emote_action) = reaction.deref().try_into() {
                            let cloned_user = *user;
                            let cloned_guild = guild.clone();
                            tokio::spawn(async move {
                                cloned_guild
                                    .main_message_event(emote_action, cloned_user)
                                    .await
                            });
                        }

                        // Delete Reaction
                        self.context
                            .scheduler
                            .process_enqueue(DeleteMessageReactionTask {
                                channel: reaction.channel_id,
                                message: reaction.message_id,
                                user: *user,
                                reaction: reaction.emoji.clone(),
                            })
                            .await
                            .ok();
                    } else {
                        // Check Message
                        let cloned_guild = guild.clone();
                        tokio::spawn(async move { cloned_guild.main_message_emote_check().await });
                    }
                }
                ReactionAction::Removed(reaction) => {
                    if let Some(user) = &reaction.user_id {
                        if !self.context.bots.contains_id(user) {
                            continue;
                        }
                    }
                    // Check Message
                    let cloned_guild = guild.clone();
                    tokio::spawn(async move { cloned_guild.main_message_emote_check().await });
                }
            }
        }

        guild
            .main_message_error(MessageError::UnexpectedEnd())
            .await
    }

    async fn update(self) {
        let mut message = self.message;
        loop {
            tokio::time::sleep(MESSAGE_UPDATE_INTERVAL).await;
            let content = Self::content(&self.context).await;
            if message.content.eq(content.as_str().unwrap()) {
                continue;
            }
            let edit_res = message
                .edit(self.bot.cache_http(), |msg| msg.content(content))
                .await;
            if edit_res.is_err() {
                //BREAK Update Loop if error occurred
                return;
            }
        }
    }

    fn duration_fmt(dur: &'_ Duration) -> String{
        let seconds = dur.as_secs() % 60;
        let minutes = (dur.as_secs() / 60) % 60;
        let hours = (dur.as_secs() / 60) / 60;
        let mut msg = String::from("");
        if hours > 0{
            write!(msg, "{:02}:", hours).unwrap();
        }
        write!(msg, "{:02}:{:02}", minutes, seconds).unwrap();
        msg
    }

    async fn content(context: &Context) -> Value {
        let mut msg: String = "```css\r\n".to_string();
        let states = context.player_manager.get_all_player_states().await;

        for state in states.iter().map(|s| s.borrow().clone()){
            if let Some(bot) = context.bots.get_bot_by_id(state.bot){
                if let Some(bot) = bot.cache().member(context.id, state.bot).await{
                    write!(msg, "[{}] {}\r\n", bot.nick.unwrap_or(bot.user.name), state.play_state.as_ref()).unwrap();
                    if let Some((dur, cur)) = &state.current{
                        write!(msg, "[CURRENT] {:.*} [{}]\r\n", SEARCH_TITLE_LIMIT, cur.track, Self::duration_fmt(dur)).unwrap();
                    }
                    for (i, track) in state.playlist.iter().enumerate().take(2){
                        write!(msg, "[{}] {:.*}\r\n", i - 1, SEARCH_TITLE_LIMIT, track.track).unwrap();
                    }
                    write!(msg, "\r\n").unwrap();
                }
            }
        }

        write!(msg, "```").unwrap();
        serde_json::Value::String(msg)
    }
}

#[derive(Error, Debug)]
pub enum MessageError {
    #[error("Serenity Error occurred: {0:?}")]
    SerenityError(SerenityError),
    #[error("Could not find Bot for Guild: {0:?}")]
    NoBot(GuildId),
    #[error("Could not find Shard for Bot: {0:?}")]
    NoShard(UserId),
    #[error("Message Timeout")]
    Timeout(),
    #[error("Unexpectedly ended")]
    UnexpectedEnd(),
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
