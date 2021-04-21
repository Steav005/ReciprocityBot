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
use crate::task_handle::DeleteMessageTask;
use lavalink_rs::LavalinkClient;
use log::{info, warn};
use serenity::model::event::ResumedEvent;
use serenity::model::prelude::{Message, VoiceState};
use serenity::FutureExt;
use std::borrow::Borrow;
use std::ops::Deref;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
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
            },
        };

        tokio::spawn(Self::clear_messages(guild.0.clone()));
        let cloned_guild = guild.clone();
        tokio::spawn(async move { cloned_guild.check_main_message().await });
        //tokio::spawn(async move {
        //    cloned_guild
        //        .main_message_error(MessageError::UnexpectedEnd())
        //        .await
        //});

        Ok(guild)
    }

    //Delete every irrelevant message
    async fn clear_messages(ctx: Context){
        //TODO hacky sleep abändern
        tokio::time::sleep(Duration::from_secs(5)).await;


        info!("Starting clear_messages in {:?}", ctx.id);
        let bot = ctx.bots.get_any_guild_bot(&ctx.id).await;
        let bot = match bot{
            None => {
                warn!("Could not get a Bot for Guild: {:?}", ctx.id);
                return;
            }
            Some(bot) => bot,
        };

        let mut i = 0;
        while let Ok(mut msgs) = ctx.channel.messages(bot.http(), |b| b.limit(200)).await {
            let s = ctx.main_message.read().await.as_ref().map(|(msg, _)| msg.message_id());
            let msgs = msgs.drain(..).filter(|m| !Some(m.id).eq(&s));
            let searches: Vec<_> = ctx.search_messages.read().await.values().copied().collect();
            let msgs: Vec<_> = msgs.filter(|m| !searches.contains(&m.id)).collect();
            if msgs.is_empty(){
                return;
            }
            info!("Attempting to delete {} Messages in Guild: {:?}", msgs.len(), ctx.id);
            let del_res = ctx.channel.delete_messages(bot.http(), msgs).await;
            if let Err(e) = del_res{
                warn!("Bulk Message Delete Error: Guild: {:?}, Error: {:?}", ctx.id, e);
            }
            if i == 4{
                return;
            }
            i += 1;
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
        info!(
            "Received Message: {}, User: {}",
            message.id, message.author.id
        );

        //Delete Message
        self.0
            .scheduler
            .process_enqueue(DeleteMessageTask {
                channel: message.channel_id,
                message: message.id,
            })
            .await
            .ok();

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
        if !same_voice_state{
            if let Err(e) = self.0.player_manager.leave(voice_channel).await{
                warn!("Error leaving. Guild: {:?}, Channel: {:?}, Error: {:?}", self.0.id, voice_channel, e);
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
            let http = bot.http().clone();
            let requester = message.author;
            let shard = match self
                .0
                .event_handler
                .get_shard_sender(self.0.id, bot.id())
                .await
            {
                None => {
                    warn!(
                        "No Shard was found for Guild/Bot. Guild: {}, Bot: {}",
                        self.0.id,
                        bot.id()
                    );
                    return;
                }
                Some(shard) => shard,
            };

            //Run the search message for determining a track
            let search_message_res = SearchMessage::search(
                http,
                songs,
                requester,
                message.content,
                shard,
                self.0.clone(),
            )
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
        if let Some((msg, _)) = self.0.main_message.read().await.deref() {
            if msg.message_id() == message {
                //Now check if the main message is alright
                self.check_main_message().await;
                return;
            }
        }

        //Check Search Messages
        let contains = self.0.search_messages.read().await.values().any(|msg| message.eq(msg));
        if contains {
            let mut search_lock = self.0.search_messages.write().await;
            if let Some(user) = search_lock.iter().find(|(_, msg)| message.eq(*msg)).map(|(user, _)| *user){
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

        info!("Checking emotes, because main message emotes were bulk deleted. Guild: {:?}, Message: {:?}", self.0.id, message);
        msg.clone().emote_check().await;
    }

    async fn resume(&self, _time: Instant, _event: ResumedEvent) {
        //TODO maybe do something, Ignore for the time being
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
                    "Ignoring Voice Update, because old one was None: Guild: {:?}",
                    self.0.id
                );
                return;
            }
            Some(st) => match st.channel_id {
                None => {
                    info!(
                        "Ignoring Voice Update, because old one was None: Guild: {:?}",
                        self.0.id
                    );
                    return;
                }
                Some(ch) => ch,
            },
        };

        //Branch if moved user is our bot
        match self.0.bots.contains_id(&new_voice_state.user_id){
            true => {
                //Only do something if the new_voice_state is a channel
                if new_voice_state.channel_id.is_some() {
                    //And if the old voice state is not non
                    if let Some(old_vc) = old_voice_state {
                        if let Some(old_ch) = old_vc.channel_id {
                            info!("Bot was moved. Guild: {:?}, Old Channel: {:?}, New Channel: {:?}, Bot: {:?}", self.0.id, old_ch, new_voice_state, &new_voice_state.user_id);
                            if let Err(e) = self.0.player_manager.leave(old_ch).await{
                                warn!("Error leaving. Guild: {:?}, Channel: {:?}, Error: {:?}", self.0.id, voice_channel, e);
                            }
                        }
                    }
                }
            }
            false => {
                //Delete Search Message if it exists
                let search = self.0.search_messages.write().await.remove(&new_voice_state.user_id);
                if let Some(msg) = search{
                    info!("Removing Search Message, because User left. Guild: {:?}, User: {:?}, Message: {:?}", self.0.id, new_voice_state.user_id, msg);
                    let del_task = DeleteMessageTask{
                        channel: self.0.channel,
                        message: msg
                    };
                    if let Err(e) = self.0.scheduler.process_enqueue(del_task).await {
                        warn!("Error queueing Search Message delete Task. Guild: {:?}, Message: {:?}, Error: {:?}", self.0.id, msg, e);
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
                            info!("Ignoring Leave, because there are still user in the channel: Guild: {:?}, Channel: {:?}", self.0.id, voice_channel);
                            return;
                        }
                    }
                }

                let leave_res = self.0.player_manager.leave(voice_channel).await;
                match leave_res {
                    Ok(_) => info!("Successfully left Channel, due to users leaving. Guild: {:?}, Channel: {:?}", self.0.id, voice_channel),
                    Err(e) => warn!("Error leaving Channel, due to users leaving. Guild: {:?}, Channel: {:?}, Error: {:?}", self.0.id, voice_channel, e),
                }
            }
        }

    }

    async fn lavalink(&self, event: LavalinkEvent, client: LavalinkClient) {
        info!("Lavaplayer Event: {:?}", &event);
        self.0
            .player_manager
            .handle_player_event(event, client)
            .await
            .ok();
    }

    //async fn main_message_error(&self, error: MessageError) {
    //    warn!("Main Message Error occurred. Guild: {:?}, Error: {:?}", self.0.id, error);
    //
    //    while let Err(e) = MainMessage::new(self.clone(), self.0.clone())
    //        .await
    //    {
    //        warn!("Error creating Main Message. Reattempting in 5 sec. Guild: {:?}, Error: {:?}", self.0.id, e);
    //        tokio::time::sleep(Duration::from_secs(5)).await;
    //    }
    //}

    async fn main_message_event(&self, event: EmoteAction, user: UserId) {
        info!(
            "Main Message Event occurred. Guild: {:?}, User: {:?}, Event: {:?}",
            self.0.id, user, event
        );

        //Ignore if User is not in a voice channel
        let voice_channel = match self.0.bots.get_user_voice_state(&user, &self.0.id).await {
            None => {
                info!(
                    "Ignoring Event because User is not in Voice Channel. Guild: {:?}. User: {:?}",
                    self.0.id, user
                );
                return;
            }
            Some(st) => match st.channel_id {
                None => {
                    info!("Ignoring Event because User is not in Voice Channel. Guild: {:?}. User: {:?}", self.0.id, user);
                    return;
                }
                Some(ch) => ch,
            },
        };

        //Build Request
        let request = match event {
            EmoteAction::PlayPause() => PlayerRequest::PauseResume(voice_channel),
            EmoteAction::Next() => PlayerRequest::Skip(voice_channel),
            EmoteAction::Prev() => PlayerRequest::BackSkip(voice_channel),
            EmoteAction::Join() => {
                let join_res = self.0.player_manager.join(voice_channel).await;
                match join_res {
                    Ok(_) => {
                        info!("Successfully joined Voice Channel. Guild: {:?}, Channel: {:?}, User: {:?}", self.0.id, voice_channel, user);
                        return;
                    }
                    Err(e) => {
                        warn!("Error joining Voice Channel. Guild: {:?}, Channel: {:?}, User: {:?}, Error: {:?}", self.0.id, voice_channel, user, e);
                        return;
                    }
                }
            }
            EmoteAction::Leave() => {
                let join_res = self.0.player_manager.leave(voice_channel).await;
                match join_res {
                    Ok(_) => {
                        info!("Successfully left Voice Channel. Guild: {:?}, Channel: {:?}, User: {:?}", self.0.id, voice_channel, user);
                        return;
                    }
                    Err(e) => {
                        warn!("Error leaving Voice Channel. Guild: {:?}, Channel: {:?}, User: {:?}, Error: {:?}", self.0.id, voice_channel, user, e);
                        return;
                    }
                }
            }
            EmoteAction::Delete() => PlayerRequest::ClearQueue(voice_channel),
            EmoteAction::LoopOne() => PlayerRequest::Playback(Playback::OneLoop, voice_channel),
            EmoteAction::LoopAll() => PlayerRequest::Playback(Playback::AllLoop, voice_channel),
            _ => {
                info!(
                    "Received unexpected Event. Guild: {:?}, User: {:?}, Event: {:?}",
                    self.0.id, user, event
                );
                return;
            }
        };

        //Run Request
        let request_res = self.0.player_manager.request(request).await;
        if let Err(e) = request_res {
            warn!(
                "Error Handling User Request. Guild: {:?}, User: {:?}, Event: {:?}, Error: {:?}",
                self.0.id, user, event, e
            );
        }
    }

    async fn check_main_message(&self) {
        info!("Start Main Message Check. Guild: {:?}", self.0.id);
        let mut message_lock = self.0.main_message.write().await;

        if let Some((msg, handle)) = message_lock.deref().borrow() {
            if msg.message_still_exists().await {
                return;
            } else {
                //Make sure Message is really gone
                info!(
                    "Making sure, Main Message is really gone. Guild: {:?}, Message: {:?}",
                    self.0.id,
                    msg.message_id()
                );
                let task = DeleteMessageTask {
                    channel: msg.channel_id(),
                    message: msg.message_id(),
                };
                let del_res = self.0.scheduler.process_enqueue(task).await;
                if let Err(e) = del_res {
                    warn!("Error queueing Main Message delete Task. Guild: {:?}, Message: {:?}, Error: {:?}", self.0.id, msg.message_id(), e);
                }
                handle.abort();
            }
        }

        loop {
            let msg_res = MainMessage::new(self.clone(), self.0.clone()).await;
            match msg_res {
                Ok((msg, run)) => {
                    info!(
                        "Created new Main Message. Guild: {:?}, Message: {:?}",
                        self.0.id,
                        msg.message_id()
                    );
                    let handle = tokio::spawn(run.boxed());
                    *message_lock = Some((msg, handle));
                    drop(message_lock);
                    return;
                }
                Err(e) => {
                    warn!("Error creating Main Message. Reattempting in 5 sec. Guild: {:?}, Error: {:?}", self.0.id, e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
