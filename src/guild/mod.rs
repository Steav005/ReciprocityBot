use std::collections::HashMap;
use std::sync::Arc;

use serenity::async_trait;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use thiserror::Error;

use crate::bots::BotMap;
use crate::config::Config;
use crate::context::{Context, GuildEventHandler};
use crate::event_handler::EventHandler;
use crate::guild::message_manager::{EmoteAction, MainMessage, SearchMessage};
use crate::guild::player_manager::{PlayerManager, PlayerMapError, PlayerRequest};
use crate::guild::scheduler::GuildScheduler;
use crate::lavalink_handler::LavalinkEvent;
use crate::player::Playback;
use crate::task_handle::DeleteMessagePoolTask;
use lavalink_rs::LavalinkClient;
use log::{debug, info, warn};
use serenity::model::event::ResumedEvent;
use serenity::model::prelude::{Message, VoiceState};
use serenity::FutureExt;
use std::borrow::Borrow;
use std::ops::Deref;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use url::Url;

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
        info!("Creating Guild: {}", id);
        let channel = config
            .guilds
            .values()
            .map(|guild| (guild.guild_id, ChannelId(guild.channel_id)))
            .find(|(guild_id, _)| *guild_id == id.0)
            .ok_or(ReciprocityGuildError::GuildNotInConfig(id))?
            .1;

        let scheduler = GuildScheduler::new(id, channel, bots.clone());
        let player_manager = Arc::new(PlayerManager::new(id, bots.clone(), lavalink));
        let search_messages = Arc::new(RwLock::new(HashMap::new()));

        let guild = ReciprocityGuild {
            0: Context {
                id,
                channel,
                bots,
                event_handler,
                config,
                scheduler,
                player_manager,
                search_messages,
                main_message: Arc::new(RwLock::new(None)),
                delete_pool: Arc::new(Mutex::new(Vec::new())),
            },
        };

        Ok(guild)
    }

    pub fn get_player_manager(&self) -> Arc<PlayerManager> {
        self.0.player_manager.clone()
    }

    pub fn get_id(&self) -> GuildId {
        self.0.id
    }

    //Delete every irrelevant message
    async fn clear_messages(ctx: Context) {
        info!("Starting clear_messages in {:?}", ctx.id);
        let bot = ctx.bots.get_any_guild_bot(&ctx.id).await;
        let bot = match bot {
            None => {
                warn!("Could not get a Bot for Guild: {:?}", ctx.id);
                return;
            }
            Some(bot) => bot,
        };

        let mut msgs = ctx.channel.messages(bot.http(), |b| b.limit(100)).await;
        loop {
            let last = if let Ok(mut msgs) = msgs {
                let last = msgs.last().cloned();
                let s = ctx
                    .main_message
                    .read()
                    .await
                    .as_ref()
                    .map(|(msg, _)| msg.message_id());
                let msgs = msgs.drain(..).filter(|m| !Some(m.id).eq(&s));
                let searches: Vec<_> = ctx.search_messages.read().await.values().copied().collect();
                let msgs: Vec<_> = msgs
                    .filter(|m| {
                        !searches
                            .iter()
                            .any(|(msg, _)| msg.map(|msg| m.id.eq(&msg)).unwrap_or(false))
                    })
                    .collect();
                if msgs.is_empty() {
                    return;
                }
                let mut msgs: Vec<_> = msgs.iter().map(|m| m.id).collect();

                info!(
                    "Attempting to delete {} Messages in Guild: {:?}",
                    msgs.len(),
                    ctx.id
                );
                ctx.delete_pool.lock().await.append(&mut msgs);
                ctx.scheduler
                    .process_enqueue(DeleteMessagePoolTask {
                        channel: ctx.channel,
                        pool: ctx.delete_pool.clone(),
                    })
                    .await
                    .ok();
                last.unwrap()
            } else {
                return;
            };
            msgs = ctx
                .channel
                .messages(bot.http(), |b| b.limit(100).after(last))
                .await;
        }
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
    async fn new_message(&self, message: Message) {
        //Ignore if the channel is wrong or the message was send by our bots
        if !self.0.channel.eq(&message.channel_id) || self.0.bots.contains_id(&message.author.id) {
            return;
        }
        info!("Received Message: {}, {}", message.id, message.author.id);

        //Delete Message with small delay
        let msg_clone = message.id;
        let ch_clone = message.channel_id;
        let delete_pool_clone = self.0.delete_pool.clone();
        let scheduler_clone = self.0.scheduler.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            delete_pool_clone.lock().await.push(msg_clone);
            scheduler_clone
                .process_enqueue(DeleteMessagePoolTask {
                    channel: ch_clone,
                    pool: delete_pool_clone,
                })
                .await
                .ok();
        });

        //Get user Voice Channel
        let voice_channel = match self
            .0
            .bots
            .get_user_voice_state(&message.author.id, &self.0.id)
            .await
        {
            None => {
                warn!("No Voice Channel for User: {}", message.author.id);
                return;
            }
            Some(vs) => match vs.channel_id {
                None => {
                    warn!("No Voice Channel for User: {}", message.author);
                    return;
                }
                Some(channel) => channel,
            },
        };

        // If No bot is in voice channel, get one
        if !self.0.player_manager.bot_in_channel(&voice_channel).await {
            let join_res = self.0.player_manager.join(voice_channel).await;
            if let Err(e) = join_res {
                match e {
                    PlayerMapError::PlayerAlreadyExists(_) => {
                        warn!(
                            "Join Failed, Player already exists for Voice Channel: {}. Proceeding",
                            voice_channel
                        )
                    }
                    _ => {
                        warn!("Join Failed, Aborting due to Error: {:?}", e);
                        return;
                    }
                }
            }
        }

        //Recheck user voice state
        let same_voice_state = match self
            .0
            .bots
            .get_user_voice_state(&message.author.id, &self.0.id)
            .await
        {
            None => false,
            Some(v) => v.channel_id.eq(&Some(voice_channel)),
        };

        //Should the voice state have changed, exit and stop player
        if !same_voice_state {
            if let Err(e) = self.0.player_manager.leave(voice_channel).await {
                warn!(
                    "Error leaving. {:?}, {:?}, {:?}",
                    self.0.id, voice_channel, e
                );
            }
            return;
        }

        //Start search
        let search_res = self
            .0
            .player_manager
            .search(voice_channel, message.content.clone())
            .await;
        let (bot, songs) = match search_res {
            Err(e) => {
                warn!("Search Error: {:?}", e);
                return;
            }
            Ok((bot, songs)) => {
                if let Some(bot) = self.0.bots.get_bot_by_id(bot) {
                    (bot, songs)
                } else {
                    warn!("No Bot was found for ID: {}", bot);
                    return;
                }
            }
        };

        //Exit if no song was found
        if songs.is_empty() {
            warn!("No Song was found for Query: {}", message.content);
            return;
        }

        //If query was not a valid url
        let tracks = if Url::parse(&message.content).is_err() {
            //Get relevant stuff for the search message
            let requester = message.author;
            let shard = match self
                .0
                .event_handler
                .get_shard_sender(self.0.id, bot.id())
                .await
            {
                None => {
                    warn!(
                        "No Shard was found for Guild/Bot. {}, Bot: {}",
                        self.0.id,
                        bot.id()
                    );
                    return;
                }
                Some(shard) => shard,
            };

            //Run the search message for determining a track
            let search_message_res =
                SearchMessage::search(songs, requester, message.content, shard, self.0.clone())
                    .await;
            match search_message_res {
                Ok(track) => vec![track],
                Err(e) => {
                    warn!("Search Message Error occurred: {:?}", e);
                    return;
                }
            }
        } else {
            songs
        };

        let enqueue_res = self
            .0
            .player_manager
            .request(PlayerRequest::Enqueue {
                0: tracks,
                1: voice_channel,
            })
            .await;
        match enqueue_res {
            Ok(_) => {}
            Err(e) => {
                warn!("Error enqueuing song: {:?}", e);
                return;
            }
        }
    }

    async fn deleted_message(&self, channel: ChannelId, message: MessageId) {
        //Exit if channel is irrelevant
        if channel != self.0.channel {
            return;
        }

        //Check Main Message
        let lock = self.0.main_message.read().await;
        if let Some(msg) = lock.as_ref().map(|(m, _)| m.clone()) {
            drop(lock);
            if msg.message_id() == message {
                //Now check if the main message is alright
                self.check_main_message().await;
                return;
            }
        } else {
            drop(lock);
        }

        //Check Search Messages
        let contains = self
            .0
            .search_messages
            .read()
            .await
            .values()
            .any(|(msg, _)| msg.map(|msg| msg.eq(&message)).unwrap_or(false));
        if contains {
            let mut search_lock = self.0.search_messages.write().await;
            if let Some(user) = search_lock
                .iter()
                .find(|(_, (msg, _))| msg.map(|msg| msg.eq(&message)).unwrap_or(false))
                .map(|(user, _)| *user)
            {
                search_lock.remove(&user);
            }
            drop(search_lock);
        }
    }

    async fn bulk_reaction_delete(&self, channel: ChannelId, message: MessageId) {
        //Exit if channel is irrelevant
        if channel != self.0.channel {
            return;
        }

        let msg = match self.0.main_message.read().await.deref().borrow() {
            None => return,
            Some((msg, _)) => msg.clone(),
        };

        //Exit if Message id is wrong
        if msg.message_id() != message {
            return;
        }

        info!(
            "Checking emotes, because main message emotes were bulk deleted. {:?}, {:?}",
            self.0.id, message
        );
        msg.clone().emote_check().await;
    }

    async fn resume(&self, time: Instant, event: ResumedEvent) {
        warn!("Resume event. {:?}, {:?}, {:?}", self.0.id, time, event);
        tokio::spawn(Self::clear_messages(self.0.clone()));
        let cloned_guild = self.clone();
        tokio::spawn(async move { cloned_guild.check_main_message().await });
    }

    async fn voice_update(
        &self,
        old_voice_state: Option<VoiceState>,
        new_voice_state: VoiceState,
        bot: UserId,
    ) {
        //Only continue if old Channel exists
        let voice_channel = match &old_voice_state {
            None => {
                info!(
                    "Ignoring Voice Update, because old one was None. {:?}",
                    self.0.id
                );
                return;
            }
            Some(st) => match st.channel_id {
                None => {
                    info!(
                        "Ignoring Voice Update, because old one was None. {:?}",
                        self.0.id
                    );
                    return;
                }
                Some(ch) => ch,
            },
        };

        //Branch if moved user is our bot
        match self.0.bots.contains_id(&new_voice_state.user_id) {
            true => {
                //Only do something if the new_voice_state is a channel
                if new_voice_state.channel_id.is_some() {
                    //And if the old voice state is not non
                    if let Some(old_vc) = old_voice_state {
                        if let Some(old_ch) = old_vc.channel_id {
                            info!(
                                "Bot was moved. {:?}, Old: {:?}, New: {:?}, Bot: {:?}",
                                self.0.id, old_ch, new_voice_state, &new_voice_state.user_id
                            );
                            if let Err(e) = self.0.player_manager.leave(old_ch).await {
                                warn!(
                                    "Error leaving. {:?}, {:?}, {:?}",
                                    self.0.id, voice_channel, e
                                );
                            }
                        }
                    }
                }
            }
            false => {
                //Delete Search Message if it exists
                let search = self
                    .0
                    .search_messages
                    .write()
                    .await
                    .remove(&new_voice_state.user_id);
                if let Some((msg, _)) = search {
                    info!(
                        "Removing Search Message, because User left. {:?}, {:?}, {:?}",
                        self.0.id, new_voice_state.user_id, msg
                    );
                    if let Some(msg) = msg {
                        self.0.delete_pool.lock().await.push(msg);
                        let del_task = DeleteMessagePoolTask {
                            channel: self.0.channel,
                            pool: self.0.delete_pool.clone(),
                        };
                        if let Err(e) = self.0.scheduler.process_enqueue(del_task).await {
                            warn!(
                                "Error queueing Search Message delete Task. {:?}, {:?}, {:?}",
                                self.0.id, msg, e
                            );
                        }
                    }
                }

                //Ignore if there are still user in the channel
                match self
                    .0
                    .bots
                    .user_in_channel_with_bot(&voice_channel, &self.0.id, bot)
                    .await
                {
                    None => return,
                    Some(in_channel) => {
                        if in_channel {
                            info!("Ignoring Leave, because there are still user in the channel. {:?}, {:?}", self.0.id, voice_channel);
                            return;
                        }
                    }
                }

                let leave_res = self.0.player_manager.leave(voice_channel).await;
                match leave_res {
                    Ok(_) => info!(
                        "Successfully left Channel, due to users leaving. {:?}, {:?}",
                        self.0.id, voice_channel
                    ),
                    Err(e) => warn!(
                        "Error leaving Channel, due to users leaving. {:?}, {:?}, {:?}",
                        self.0.id, voice_channel, e
                    ),
                }
            }
        }
    }

    async fn lavalink(&self, event: LavalinkEvent, client: LavalinkClient) {
        debug!("Lavaplayer Event: {:?}", &event);
        self.0
            .player_manager
            .handle_player_event(event, client)
            .await
            .ok();
    }

    async fn main_message_event(&self, event: EmoteAction, user: UserId) {
        info!(
            "Main Message Event occurred. {:?}, {:?}, {:?}",
            self.0.id, user, event
        );

        //Ignore if User is not in a voice channel
        let voice_channel = match self.0.bots.get_user_voice_state(&user, &self.0.id).await {
            None => {
                info!(
                    "Ignoring Event because User is not in Voice Channel. {:?}. {:?}",
                    self.0.id, user
                );
                return;
            }
            Some(st) => match st.channel_id {
                None => {
                    info!(
                        "Ignoring Event because User is not in Voice Channel. {:?}. {:?}",
                        self.0.id, user
                    );
                    return;
                }
                Some(ch) => ch,
            },
        };

        //Build Request
        let request = match event {
            EmoteAction::PlayPause() => PlayerRequest::PauseResume(voice_channel),
            EmoteAction::Next() => PlayerRequest::Skip(1, voice_channel),
            EmoteAction::Prev() => PlayerRequest::BackSkip(1, voice_channel),
            EmoteAction::Join() => {
                let join_res = self.0.player_manager.join(voice_channel).await;
                match join_res {
                    Ok(_) => {
                        info!(
                            "Successfully joined Voice Channel. {:?}, {:?}, {:?}",
                            self.0.id, voice_channel, user
                        );
                        return;
                    }
                    Err(e) => {
                        warn!(
                            "Error joining Voice Channel. {:?}, {:?}, {:?}, {:?}",
                            self.0.id, voice_channel, user, e
                        );
                        return;
                    }
                }
            }
            EmoteAction::Leave() => {
                let join_res = self.0.player_manager.leave(voice_channel).await;
                match join_res {
                    Ok(_) => {
                        info!(
                            "Successfully left Voice Channel. {:?}, {:?}, {:?}",
                            self.0.id, voice_channel, user
                        );
                        return;
                    }
                    Err(e) => {
                        warn!(
                            "Error leaving Voice Channel. {:?}, {:?}, {:?}, {:?}",
                            self.0.id, voice_channel, user, e
                        );
                        return;
                    }
                }
            }
            EmoteAction::Delete() => PlayerRequest::ClearQueue(voice_channel),
            EmoteAction::LoopOne() => PlayerRequest::Playback(Playback::OneLoop, voice_channel),
            EmoteAction::LoopAll() => PlayerRequest::Playback(Playback::AllLoop, voice_channel),
            _ => {
                info!(
                    "Received unexpected Event. {:?}, {:?}, {:?}",
                    self.0.id, user, event
                );
                return;
            }
        };

        //Run Request
        let request_res = self.0.player_manager.request(request).await;
        if let Err(e) = request_res {
            warn!(
                "Error Handling User Request. {:?}, {:?}, {:?}, {:?}",
                self.0.id, user, event, e
            );
        }
    }

    async fn check_main_message(&self) {
        info!("Start Main Message Check. {:?}", self.0.id);
        let mut message_lock = self.0.main_message.write().await;

        if let Some((msg, handle)) = message_lock.deref().borrow() {
            if msg.message_still_exists().await {
                info!("Main Message still exists. {:?}", self.0.id);
                return;
            } else {
                //Make sure Message is really gone
                info!(
                    "Making sure, Main Message is really gone. {:?}, {:?}",
                    self.0.id,
                    msg.message_id()
                );
                self.0.delete_pool.lock().await.push(msg.message_id());
                let task = DeleteMessagePoolTask {
                    channel: msg.channel_id(),
                    pool: self.0.delete_pool.clone(),
                };
                let del_res = self.0.scheduler.process_enqueue(task).await;
                if let Err(e) = del_res {
                    warn!(
                        "Error queueing Main Message delete Task. {:?}, {:?}, {:?}",
                        self.0.id,
                        msg.message_id(),
                        e
                    );
                }
                handle.abort();
            }
        } else {
            warn!("Main Message was empty. {:?}", self.0.id);
        }

        loop {
            let msg_res = MainMessage::new(self.clone(), self.0.clone()).await;
            match msg_res {
                Ok((msg, run)) => {
                    info!(
                        "Created new Main Message. {:?}, {:?}",
                        self.0.id,
                        msg.message_id()
                    );
                    let handle = tokio::spawn(run.boxed());
                    *message_lock = Some((msg, handle));
                    drop(message_lock);
                    return;
                }
                Err(e) => {
                    warn!(
                        "Error creating Main Message. Reattempting in 5 sec. {:?}, {:?}",
                        self.0.id, e
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn cache_ready(&self, bot: UserId) {
        info!("Cache Ready. {:?}, Bot: {:?}", self.0.id, bot);
        tokio::spawn(Self::clear_messages(self.0.clone()));
        let cloned_guild = self.clone();
        tokio::spawn(async move { cloned_guild.check_main_message().await });
    }
}
