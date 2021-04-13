use crate::bots::{Bot, BotMap};
use crate::event_handler::EventHandler;
use crate::guild::player_manager::{PlayerManager, PlayerRequest, PlayerStates};
use crate::guild::scheduler::GuildScheduler;
use crate::player::PlayerState;
use crate::task_handle::{AddMessageReactionTask, DeleteMessageReactionTask, DeleteMessageTask};
use futures::future::{BoxFuture, Either, Select, SelectAll};
use futures::FutureExt;
use futures::StreamExt;
use lavalink_rs::model::{Play, Track};
use serde_json::Value;
use serenity::client::bridge::gateway::ShardMessenger;
use serenity::collector::{ReactionAction, ReactionCollector};
use serenity::http::Http;
use serenity::model::guild::Target::Emoji;
use serenity::model::prelude::{ChannelId, GuildId, Message, MessageId, Reaction, ReactionType, UserId, VoiceState};
use serenity::model::Permissions;
use serenity::prelude::SerenityError;
use serenity::utils::MessageBuilder;
use std::borrow::{Borrow, BorrowMut};
use std::convert::TryFrom;
use std::future::Future;
use std::iter::Map;
use std::ops::{Deref, Index};
use std::pin::Pin;
use std::slice::IterMut;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::watch::error::RecvError;
use tokio::sync::watch::{Receiver as WatchReceiver, Receiver};
use serenity::model::guild::Member;

const DELETE_MESSAGE_DELAY: Duration = Duration::from_millis(500);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
const SEARCH_TITLE_LIMIT: usize = 40;

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
        channel: ChannelId,
        tracks: Vec<Track>,
        requester: UserId,
        shard_messenger: impl AsRef<ShardMessenger>,
        scheduler: GuildScheduler,
    ) -> Result<Track, MessageError> {
        let message = bot
            .send_message(channel.0, &Self::content(&tracks))
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
            channel,
            emotes_1.clone(),
            scheduler.clone(),
        ));
        let track: Result<Track, MessageError> = collector.await.ok_or(MessageError::Timeout());

        scheduler
            .process_enqueue(DeleteMessageTask {
                channel,
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
pub struct MainMessage{
    guild: GuildId,
    channel: ChannelId,
    bots: Arc<BotMap>,
    bot: Arc<Bot>,
    shard: ShardMessenger,
    player_manager: PlayerManager,
    event_handler: EventHandler,
    scheduler: GuildScheduler,
}

impl MainMessage {
    pub async fn run(
        guild: GuildId,
        channel: ChannelId,
        bots: Arc<BotMap>,
        player_manager: PlayerManager,
        event_handler: EventHandler,
        scheduler: GuildScheduler,
    ) -> MessageError {
        if let Some(bot) = bots.get_any_guild_bot(&guild).await {
            if let Some(shard) = event_handler.get_shard_sender(guild, bot.id()).await {
                let main = MainMessage{
                    guild,
                    channel,
                    bots,
                    bot,
                    shard,
                    player_manager,
                    event_handler,
                    scheduler
                };


                return match main.run_internal()
                    .await
                {
                    Ok(_) => MessageError::UnexpectedEnd(),
                    Err(s) => MessageError::SerenityError(s),
                };
            }
            return MessageError::NoShard(bot.id());
        }
        MessageError::NoBot(guild)
    }

    async fn run_internal(
        self
    ) -> Result<(), SerenityError> {
        let player = self.player_manager.get_all_player_states().await;
        let message = self.bot
            .http()
            .send_message(self.channel.0, &self.generate_content())
            .await?;

        let update_handle =
            tokio::spawn(self.clone().run_update(message.id));

        let emotes: Arc<Vec<_>> = Arc::new(vec![
            EmoteAction::Prev(),
            EmoteAction::PlayPause(),
            EmoteAction::Next(),
            EmoteAction::LoopOne(),
            EmoteAction::LoopAll(),
            EmoteAction::Join(),
            EmoteAction::Leave(),
            EmoteAction::Nothing(),
        ]);

        //Build Collector
        let mut collector = message
            .await_reactions(&self.shard)
            .removed(true)
            .added(true)
            .await;
        tokio::spawn(add_emotes(
            message.id,
            self.channel,
            emotes.clone(),
            self.scheduler.clone(),
        ));

        while let Some(reaction_action) = collector.next().await {
            let action = EmoteAction::try_from(reaction_action.as_inner_ref().deref())
                .ok()
                .filter(|p| emotes.contains(p));
            tokio::spawn(Self::handle_reaction_action(
                self.clone(),
                reaction_action,
                action,
                message.id,
            ));
        }

        update_handle.abort();

        Ok(())
    }

    async fn handle_reaction_action(
        self,
        reaction: Arc<ReactionAction>,
        action: Option<EmoteAction>,
        message: MessageId,
    ) {
        if let ReactionAction::Added(reaction) = reaction.as_ref() {
            if let Some(user) = reaction.user_id {
                if let Some(user) = self.bot.cache().user(user).await {
                    if user.bot {
                        return;
                    }
                }
            }

            self.scheduler.process_enqueue(DeleteMessageReactionTask{
                channel: self.channel,
                message,
                user: reaction.user_id.unwrap_or_default(),
                reaction: reaction.emoji.clone()
            }).await.ok();

            //if let Some(user) = reaction.user_id{
            //    if let Some() = self.bots.get_user_voice_state(&user, &self.guild).await
            //}

            if let Some(action) = action{
                match action{
                    EmoteAction::PlayPause() => {}
                    EmoteAction::Next() => {}
                    EmoteAction::Prev() => {}
                    EmoteAction::Join() => {}
                    EmoteAction::Leave() => {}
                    EmoteAction::Delete() => {}
                    EmoteAction::LoopOne() => {}
                    EmoteAction::LoopAll() => {}
                    EmoteAction::Nothing() => {}
                    _ => {}
                }
            }
        }
        todo!("What should be done if a bot emote was deleted")
    }

    async fn run_update(
        self,
        message: MessageId,
    ) -> SerenityError {
        //Prepare cloned player
        let mut player = self.player_manager.get_all_player_states().await;
        let cloned_player = player.clone();

        //Pre clear changed
        player
            .changed()
            .await
            .expect("PlayerManager Status Sender dropped");
        let mut players = player.borrow().clone();
        //Pre clear changed for all players
        let players_changed = players.iter_mut().map(|p| p.changed().boxed());
        drop(futures::future::join_all(players_changed).await);

        loop {
            //create new changed for list and every single player
            let players_changed = players.iter_mut().map(|p| p.changed().boxed());
            let player_changed = player.changed().boxed();

            //Update Message
            if let Err(s) = self.bot
                .cache_http()
                .http
                .edit_message(
                    self.channel.0,
                    message.0,
                    &Self::generate_content(&self),
                )
                .await
            {
                return s;
            }
            //Await any Change
            let changed = futures::future::select(
                player_changed,
                futures::future::select_all(players_changed),
            )
            .await;

            //Check what changed
            players = match changed {
                Either::Left(_) => {
                    drop(changed);
                    //Create new players if list changed
                    let mut players = player.borrow().clone();
                    //Pre clear changed
                    let players_changed = players.iter_mut().map(|p| p.changed().boxed());
                    drop(futures::future::join_all(players_changed).await);
                    players
                }
                Either::Right(_) => {
                    drop(changed);
                    players
                }
            };
        }
    }

    fn generate_content(&self) -> Value {
        todo!()
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
