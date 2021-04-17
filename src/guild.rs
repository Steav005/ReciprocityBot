use std::collections::HashMap;
use std::sync::Arc;

use serenity::async_trait;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use thiserror::Error;

use log::{info, debug, warn};
use crate::bots::BotMap;
use crate::config::Config;
use crate::context::{Context, GuildEventHandler};
use crate::event_handler::EventHandler;
use crate::guild::message_manager::{EmoteAction, MainMessage, MessageError, SearchMessage};
use crate::guild::player_manager::{PlayerManager, PlayerMapError, PlayerRequest};
use crate::guild::scheduler::GuildScheduler;
use crate::lavalink_handler::LavalinkEvent;
use crate::player::Playback;
use crate::task_handle::DeleteMessageTask;
use lavalink_rs::LavalinkClient;
use serenity::model::event::ResumedEvent;
use serenity::model::prelude::{Message, VoiceState};
use std::time::{Duration, Instant};

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

        let guild = ReciprocityGuild {
            0: Context {
                id,
                channel,
                bots,
                event_handler,
                config,
                scheduler,
                player_manager,
            },
        };

        let cloned_guild = guild.clone();
        tokio::spawn(async move {
            cloned_guild
                .main_message_error(MessageError::UnexpectedEnd())
                .await
        });

        Ok(guild)
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
        info!("Received Message: {}, User: {}", message.id, message.author.id);

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
                debug!("No Voice Channel for User: {}", message.author.id);
                return;
            },
            Some(vs) => match vs.channel_id {
                None => {
                    debug!("No Voice Channel for User: {}", message.author);
                    return;
                },
                Some(channel) => channel,
            },
        };

        // If No bot is in voice channel, get one
        if !self.0.player_manager.bot_in_channel(&voice_channel).await {
            let join_res = self.0.player_manager.join(voice_channel).await;
            if let Err(e) = join_res {
                match e {
                    PlayerMapError::PlayerAlreadyExists(_) => {
                        debug!("Join Failed, Player already exists for Voice Channel: {}. Proceeding", voice_channel)
                    }
                    _ => {
                        warn!("Join Failed, Aborting due to Error: {:?}", e);
                        return;
                    },
                }
            }
        }

        //Start search
        let search_res = self
            .0
            .player_manager
            .search(voice_channel, message.content)
            .await;
        let (bot, songs) = match search_res {
            Err(e) => {
                warn!("Search Error: {:?}", e);
                return;
            },
            Ok((bot, songs)) => {
                if let Some(bot) = self.0.bots.get_bot_by_id(bot) {
                    (bot, songs)
                } else {
                    warn!("No Bot was found for ID: {}", bot);
                    return;
                }
            }
        };

        //Get relevant stuff for the search message
        let http = bot.http().clone();
        let requester = message.author.id;
        let shard = match self
            .0
            .event_handler
            .get_shard_sender(self.0.id, bot.id())
            .await
        {
            None => {
                warn!("No Shard was found for Guild/Bot. Guild: {}, Bot: {}", self.0.id, bot.id());
                return;
            },
            Some(shard) => shard,
        };

        //Run the search message for determining a track
        let search_message_res =
            SearchMessage::search(http, songs, requester, shard, self.0.clone()).await;
        let track = match search_message_res {
            Ok(track) => track,
            Err(e) => {
                warn!("Search Message Error occurred: {:?}", e);
                return;
            },
        };

        let enqueue_res = self
            .0
            .player_manager
            .request(PlayerRequest::Enqueue {
                0: track,
                1: voice_channel,
            })
            .await;
        match enqueue_res {
            Ok(_) => return,
            Err(e) => {
                warn!("Error enqueuing song: {:?}", e);
                return;
            },
        }
    }

    async fn deleted_message(&self, _channel: ChannelId, _message: MessageId) {
        //TODO maybe do something, Ignore for the time being
    }

    async fn resume(&self, _time: Instant, _event: ResumedEvent) {
        //TODO maybe do something, Ignore for the time being
    }

    async fn voice_update(
        &self,
        old_voice_state: Option<VoiceState>,
        _new_voice_state: VoiceState,
    ) {
        //TODO Log

        //Only continue if old Channel exists
        let voice_channel = match old_voice_state {
            None => return,
            Some(st) => match st.channel_id {
                None => return,
                Some(ch) => ch,
            },
        };

        //Ignore if there are still user in the channel
        if self
            .0
            .bots
            .user_in_channel(&voice_channel, &self.0.id)
            .await
        {
            return;
        }

        let leave_res = self.0.player_manager.leave(voice_channel).await;
        match leave_res {
            Ok(_) => return,
            Err(_) => return,
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

    async fn main_message_error(&self, _error: MessageError) {
        //TODO Log

        while MainMessage::new(self.clone(), self.0.clone())
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn main_message_event(&self, event: EmoteAction, user: UserId) {
        //TODO Log

        //Ignore if User is not in a voice channel
        let voice_channel = match self.0.bots.get_user_voice_state(&user, &self.0.id).await {
            None => return,
            Some(st) => match st.channel_id {
                None => return,
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
                    Ok(_) => return,
                    Err(_) => return,
                }
            }
            EmoteAction::Leave() => {
                let join_res = self.0.player_manager.leave(voice_channel).await;
                match join_res {
                    Ok(_) => return,
                    Err(_) => return,
                }
            }
            EmoteAction::Delete() => PlayerRequest::ClearQueue(voice_channel),
            EmoteAction::LoopOne() => PlayerRequest::Playback(Playback::OneLoop, voice_channel),
            EmoteAction::LoopAll() => PlayerRequest::Playback(Playback::AllLoop, voice_channel),
            _ => return,
        };

        //Run Request
        let request_res = self.0.player_manager.request(request).await;
        match request_res {
            Ok(_) => return,
            Err(_) => return,
        }
    }
}
