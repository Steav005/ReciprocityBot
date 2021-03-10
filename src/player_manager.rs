use crate::lavalink_handler::{GuildLavalinkHandler, LavalinkEvent};
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::player::{Playback, Player, PlayerError};
use lavalink_rs::model::Track;
use lavalink_rs::LavalinkClient;
use log::{error, warn};
use rand::seq::SliceRandom;
use serenity::model::prelude::{ChannelId, GuildId, UserId};
use serenity::{async_trait, CacheAndHttp};
use songbird::Songbird;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::{RwLock, RwLockWriteGuard};

#[derive(Clone)]
pub struct PlayerManager {
    guild: GuildId,
    bots: HashMap<UserId, (Arc<CacheAndHttp>, Arc<Songbird>)>,
    supervisor: LavalinkSupervisor,
    player_map: HashMap<UserId, Arc<RwLock<Option<Player>>>>,
}

impl PlayerManager {
    pub fn new(
        guild: GuildId,
        bots: HashMap<UserId, (Arc<CacheAndHttp>, Arc<Songbird>)>,
        supervisor: LavalinkSupervisor,
    ) -> PlayerManager {
        let player_map: HashMap<_, _> = bots
            .keys()
            .map(|id| (*id, Arc::new(RwLock::new(None))))
            .collect();

        PlayerManager {
            guild,
            bots,
            supervisor,
            player_map,
        }
    }

    pub async fn request(&self, request: PlayerRequest) -> Result<(), PlayerMapError> {
        if let PlayerRequest::Join(channel) = request {
            self.join_free_bot(channel).await
        } else {
            if let Some((_, player)) = self.get_player_channel(request.get_channel()).await {
                let mut player = player.write().await;
                let no_player_error = PlayerMapError::NoPlayerFound(request.get_channel());

                return match request {
                    PlayerRequest::Join(_) => Ok(()),
                    PlayerRequest::Leave(_) => {
                        player.take().ok_or(no_player_error)?.disconnect().await
                    }
                    PlayerRequest::Skip(_) => {
                        (*player).as_mut().ok_or(no_player_error)?.skip().await;
                        Ok(())
                    }
                    PlayerRequest::BackSkip(_) => {
                        (*player).as_mut().ok_or(no_player_error)?.back_skip().await;
                        Ok(())
                    }
                    PlayerRequest::ClearQueue(_) => {
                        (*player).as_mut().ok_or(no_player_error)?.clear_queue();
                        Ok(())
                    }
                    PlayerRequest::Playback(playback, _) => {
                        (*player)
                            .as_mut()
                            .ok_or(no_player_error)?
                            .playback(playback);
                        Ok(())
                    }
                    PlayerRequest::PauseResume(_) => {
                        (*player)
                            .as_mut()
                            .ok_or(no_player_error)?
                            .dynamic_pause_resume()
                            .await
                    }
                }
                .map_err(PlayerMapError::PlayerError);
            }
            Ok(())
        }
    }

    pub async fn search(
        &self,
        channel: ChannelId,
        query: String,
    ) -> Result<Vec<Track>, PlayerMapError> {
        let (_, player) = self
            .get_player_channel(channel)
            .await
            .ok_or(PlayerMapError::NoPlayerFound(channel))?;
        let (send, rev) = tokio::sync::oneshot::channel();
        player
            .read()
            .await
            .as_ref()
            .ok_or(PlayerMapError::NoPlayerFound(channel))?
            .search(query, |res| async { if send.send(res).is_ok() {} });

        match rev.await {
            Ok(res) => res.map_err(PlayerMapError::PlayerError),
            Err(rec_err) => Err(PlayerMapError::SearchSenderDropped(rec_err)),
        }
    }

    async fn join_free_bot(&self, channel: ChannelId) -> Result<(), PlayerMapError> {
        let mut bot_vec = self.player_map.iter().collect::<Vec<_>>();
        let mut rng = rand::thread_rng();
        bot_vec.shuffle(&mut rng);

        for (bot, player) in bot_vec {
            if player.read().await.is_none() {
                if let Some((cache_http, songbird)) = self.bots.get(bot) {
                    if cache_http
                        .cache
                        .guild_field(self.guild, |_| ())
                        .await
                        .is_some()
                    {
                        let mut lock = player.write().await;
                        if lock.is_some() {
                            continue;
                        }
                        if let Some(lavalink) = self.supervisor.request_current(*bot).await {
                            *lock = Some(
                                Player::new(channel, self.guild, songbird.clone(), lavalink)
                                    .await
                                    .map_err(PlayerMapError::PlayerError)?,
                            );
                            return Ok(());
                        }
                    }
                }
            }
        }

        Err(PlayerMapError::NoFreeBot())
    }

    pub async fn has_player(&self, channel: ChannelId) -> bool {
        self.get_player_channel(channel).await.is_some()
    }

    async fn get_player_channel(
        &self,
        channel: ChannelId,
    ) -> Option<(UserId, Arc<RwLock<Option<Player>>>)> {
        for (bot, player_lock) in self.player_map.iter() {
            if let Some(player) = player_lock.read().await.as_ref() {
                if channel == player.get_channel() {
                    return Some((*bot, player_lock.clone()));
                }
            }
        }
        None
    }

    async fn get_player_lavalink(
        &self,
        lavalink: &LavalinkClient,
    ) -> Option<(UserId, Arc<RwLock<Option<Player>>>)> {
        for (bot, player_lock) in self.player_map.iter() {
            if let Some(player) = player_lock.read().await.as_ref() {
                if Arc::ptr_eq(&lavalink.inner, &player.get_lavalink().inner) {
                    return Some((*bot, player_lock.clone()));
                }
            }
        }
        None
    }

    async fn handle_error(
        &self,
        error: PlayerError,
        bot: UserId,
        error_lavalink: LavalinkClient,
        mut lock: RwLockWriteGuard<'_, Option<Player>>,
    ) {
        //Check if error is indeed a lavalink error
        if error.is_lavalink_error() {
            //Check if a player exists
            if let Some(player) = lock.take() {
                //If it exists try disconnecting
                if player.disconnect().await.is_ok() {
                    //If it manages to disconnect there is no error, so we warn and exit
                    warn!(
                        "Error occurred earlier, but disconnect was successful, {:?}",
                        error
                    );
                    return;
                }
            }
            //Drop player map lock for others
            drop(lock);

            //Generate new Lavalink
            if self
                .supervisor
                .request_new(bot, error_lavalink.clone())
                .await
                .is_some()
            {
                //Ignore
            }
        } else {
            warn!("{:?}", error);
        }
    }
}

#[async_trait]
impl GuildLavalinkHandler for PlayerManager {
    async fn run(&self, event: LavalinkEvent) {
        if let Some((bot, player_lock)) = self.get_player_lavalink(event.get_client()).await {
            let mut player_lock = player_lock.write().await;
            if let Some(player) = player_lock.as_mut() {
                match event {
                    LavalinkEvent::PlayerUpdate(update, _) => player.update(update),
                    LavalinkEvent::TrackStart(start, _) => player.track_start(start),
                    LavalinkEvent::TrackFinish(finish, lavalink) => {
                        if let Err(err) = player.track_end(finish).await {
                            self.handle_error(err, bot, lavalink, player_lock).await;
                        }
                    }
                }
            }
        }
    }
}

pub enum PlayerRequest {
    Join(ChannelId),
    Leave(ChannelId),
    Skip(ChannelId),
    BackSkip(ChannelId),
    ClearQueue(ChannelId),
    Playback(Playback, ChannelId),
    PauseResume(ChannelId),
}

impl PlayerRequest {
    pub fn get_channel(&self) -> ChannelId {
        match self {
            PlayerRequest::Join(channel) => *channel,
            PlayerRequest::Leave(channel) => *channel,
            PlayerRequest::Skip(channel) => *channel,
            PlayerRequest::BackSkip(channel) => *channel,
            PlayerRequest::ClearQueue(channel) => *channel,
            PlayerRequest::Playback(_, channel) => *channel,
            PlayerRequest::PauseResume(channel) => *channel,
        }
    }
}

#[derive(Error, Debug)]
pub enum PlayerMapError {
    #[error("Bot is missing in Supervisor: {0:?}")]
    BotMissingInSupervisor(UserId),
    #[error("Could not find Player for: {0:?}")]
    NoPlayerFound(ChannelId),
    #[error("Search Sender was dropped: {0:?}")]
    SearchSenderDropped(RecvError),
    #[error("Player Error occurred: {0:?}")]
    PlayerError(PlayerError),
    #[error("No free Bot available")]
    NoFreeBot(),
}
