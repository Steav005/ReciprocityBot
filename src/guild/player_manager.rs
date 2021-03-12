use crate::guild::ReciprocityGuild;
use crate::player::{Playback, Player, PlayerError, PlayerState};
use arc_swap::ArcSwap;
use lavalink_rs::model::Track;
use lavalink_rs::LavalinkClient;
use log::error;
use rand::seq::SliceRandom;
use serenity::async_trait;
use serenity::model::prelude::{ChannelId, UserId};
use std::borrow::BorrowMut;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::RwLock;

pub type PlayerMap = Arc<ArcSwap<HashMap<UserId, Arc<RwLock<Option<Player>>>>>>;
pub type PlayerStateMap = Arc<RwLock<HashMap<ChannelId, Arc<PlayerState>>>>;

#[async_trait]
pub trait PlayerManager: Sync + Send {
    ///Initiates Player Map with RwLocks for every bot UserID
    fn init_player_manager(&mut self);

    ///Processes PlayerRequest
    /// - Returns Error if any occurs
    /// - If Fatal Error occurs, disconnects player and requests new lavalink
    /// - If no Error occurs, call handle_fatal_error
    async fn request(&self, request: PlayerRequest) -> Result<(), PlayerMapError>;

    ///Performs a search with the help of a specific player
    /// - Returns list of Tracks
    /// - Returns error if any occurs
    /// - If Fatal Error occurs, call handle_fatal_error
    async fn search(&self, channel: ChannelId, query: String)
        -> Result<Vec<Track>, PlayerMapError>;

    ///Handles a fatal Error
    /// - Disconnects Player if it exists
    /// - Requests new Lavalink
    async fn handle_fatal_error(&self, player: Arc<RwLock<Option<Player>>>);

    ///Let free bot join specific channel
    /// - Chooses Random Bot and lets it join a channel
    /// - Returns Error if any occurs
    /// - If no Error occurs, adds player to player map
    async fn join_free_bot(&self, channel: ChannelId) -> Result<PlayerState, PlayerMapError>;

    ///Updates Playerstate with new one
    /// - If player_state is None, does nothing
    /// - If player_state does not exist yet, creates it
    async fn update_player_state(&self, channel: ChannelId, player_state: Option<PlayerState>);

    ///Returns clones Map of all current player states
    async fn get_all_player_states(&self) -> HashMap<ChannelId, Arc<PlayerState>>;

    /// Get Player by channel
    /// - Returns None, if none is found
    async fn get_player_channel(
        &self,
        channel: ChannelId,
    ) -> Option<(UserId, Arc<RwLock<Option<Player>>>)>;

    /// Get Player by Lavalink
    /// - Returns None, if none is found
    async fn get_player_lavalink(
        &self,
        lavalink: &LavalinkClient,
    ) -> Option<(UserId, Arc<RwLock<Option<Player>>>)>;
}

#[async_trait]
impl PlayerManager for ReciprocityGuild {
    fn init_player_manager(&mut self) {
        self.player_map.store(Arc::new(
            self.bots
                .keys()
                .map(|id| (*id, Arc::new(RwLock::new(None))))
                .collect(),
        ));
    }

    async fn request(&self, request: PlayerRequest) -> Result<(), PlayerMapError> {
        let player = self.get_player_channel(request.get_channel()).await;
        let player_1 = player.clone();
        let no_player_error = PlayerMapError::NoPlayerFound(request.get_channel());
        let no_bot_error = PlayerMapError::NoFreeBot();

        let result = match request {
            PlayerRequest::Join(channel) => self.join_free_bot(channel).await.map(Some),
            PlayerRequest::Leave(_) => {
                let (bot, player) = player.ok_or(no_bot_error)?;
                let player = player.write().await.take().ok_or(no_player_error)?;
                player
                    .disconnect()
                    .await
                    .map(|_| None)
                    .map_err(PlayerMapError::PlayerError)
            }
            PlayerRequest::Skip(_) => {
                let (_, player) = player.ok_or(no_bot_error)?;
                let mut player_lock = player.write().await;
                let mut player = player_lock.deref_mut().as_mut().ok_or(no_player_error)?;
                player.skip().await.map_err(PlayerMapError::PlayerError)
            }
            PlayerRequest::BackSkip(_) => {
                let (_, player) = player.ok_or(no_bot_error)?;
                let mut player_lock = player.write().await;
                let mut player = player_lock.deref_mut().as_mut().ok_or(no_player_error)?;
                player
                    .back_skip()
                    .await
                    .map_err(PlayerMapError::PlayerError)
            }
            PlayerRequest::ClearQueue(_) => {
                let (_, player) = player.ok_or(no_bot_error)?;
                let mut player_lock = player.write().await;
                let mut player = player_lock.deref_mut().as_mut().ok_or(no_player_error)?;
                Ok(player.clear_queue())
            }
            PlayerRequest::Playback(playback, _) => {
                let (_, player) = player.ok_or(no_bot_error)?;
                let mut player_lock = player.write().await;
                let mut player = player_lock.deref_mut().as_mut().ok_or(no_player_error)?;
                Ok(player.playback(playback)).map(Some)
            }
            PlayerRequest::PauseResume(_) => {
                let (_, player) = player.ok_or(no_bot_error)?;
                let mut player_lock = player.write().await;
                let mut player = player_lock.deref_mut().as_mut().ok_or(no_player_error)?;
                player
                    .dynamic_pause_resume()
                    .await
                    .map(Some)
                    .map_err(PlayerMapError::PlayerError)
            }
        };

        //Check request result
        match result {
            Ok(state) => {
                self.update_player_state(request.get_channel(), state).await;
                Ok(())
            }
            Err(err) => {
                if let Some((_, player)) = player_1 {
                    if err.is_fatal() {
                        self.handle_fatal_error(player).await;
                    }
                }
                Err(err)
            }
        }
    }

    async fn search(
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
            Ok(res) => {
                if let Err(e) = &res {
                    if e.is_fatal() {
                        self.handle_fatal_error(player).await;
                    }
                }
                res.map_err(PlayerMapError::PlayerError)
            }
            Err(rec_err) => Err(PlayerMapError::SearchSenderDropped(rec_err)),
        }
    }

    async fn handle_fatal_error(&self, player: Arc<RwLock<Option<Player>>>) {
        if let Some(player) = player.write().await.take() {
            let channel = player.get_channel();
            player.disconnect().await.ok();
            self.player_state_map.write().await.remove(&channel);
        };
    }

    async fn join_free_bot(&self, channel: ChannelId) -> Result<PlayerState, PlayerMapError> {
        let mut bot_vec = (*self.player_map.load_full())
            .clone()
            .drain()
            .collect::<Vec<_>>();
        bot_vec.shuffle(rand::thread_rng().borrow_mut());

        for (bot, player) in bot_vec {
            if player.read().await.is_none() {
                if let Some((cache_http, songbird)) = self.bots.get(&bot) {
                    if cache_http
                        .cache
                        .guild_field(self.id, |_| ())
                        .await
                        .is_some()
                    {
                        let mut lock = player.write().await;
                        if lock.is_some() {
                            continue;
                        }
                        if let Some(lavalink) = self.lavalink_supervisor.request_current(bot).await
                        {
                            let (player, state) =
                                Player::new(bot, channel, self.id, songbird.clone(), lavalink)
                                    .await
                                    .map_err(PlayerMapError::PlayerError)?;
                            *lock = Some(player);
                            return Ok(state);
                        }
                    }
                }
            }
        }

        Err(PlayerMapError::NoFreeBot())
    }

    async fn update_player_state(&self, channel: ChannelId, player_state: Option<PlayerState>) {
        if let Some(player_state) = player_state {
            self.player_state_map
                .write()
                .await
                .insert(channel, Arc::new(player_state));
            self.player_state_map_notify.notify_waiters();
        }
    }

    async fn get_all_player_states(&self) -> HashMap<ChannelId, Arc<PlayerState>, RandomState> {
        self.player_state_map.read().await.clone()
    }

    async fn get_player_channel(
        &self,
        channel: ChannelId,
    ) -> Option<(UserId, Arc<RwLock<Option<Player>>>)> {
        for (bot, player_lock) in self.player_map.load().iter() {
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
        for (bot, player_lock) in self.player_map.load().iter() {
            if let Some(player) = player_lock.read().await.as_ref() {
                if Arc::ptr_eq(&lavalink.inner, &player.get_lavalink().inner) {
                    return Some((*bot, player_lock.clone()));
                }
            }
        }
        None
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
    #[error("Could not find Player for: {0:?}")]
    NoPlayerFound(ChannelId),
    #[error("Search Sender was dropped: {0:?}")]
    SearchSenderDropped(RecvError),
    #[error("Player Error occurred: {0:?}")]
    PlayerError(PlayerError),
    #[error("No free Bot available")]
    NoFreeBot(),
}

impl PlayerMapError {
    pub fn is_fatal(&self) -> bool {
        if let PlayerMapError::PlayerError(error) = self {
            return error.is_fatal();
        }
        false
    }
}
