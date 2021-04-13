use crate::bots::BotMap;
use crate::guild::message_manager::EmoteAction;
use crate::guild::ReciprocityGuild;
use crate::lavalink_handler::LavalinkEvent;
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::player::{Playback, Player, PlayerError, PlayerState};
use arc_swap::ArcSwap;
use lavalink_rs::model::Track;
use lavalink_rs::LavalinkClient;
use log::error;
use rand::seq::SliceRandom;
use serenity::async_trait;
use serenity::model::prelude::{ChannelId, GuildId, UserId};
use std::borrow::BorrowMut;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::watch::{Receiver as WatchReceiver, Sender as WatchSender};
use tokio::sync::{Mutex, Notify, RwLock};

pub type PlayerStates = Vec<WatchReceiver<Arc<PlayerState>>>;

#[derive(Clone)]
pub struct PlayerManager {
    guild: GuildId,
    bots: Arc<BotMap>,
    player_states_sender: Arc<Mutex<WatchSender<PlayerStates>>>,
    player_states_receiver: WatchReceiver<PlayerStates>,
    player: Arc<HashMap<UserId, Arc<RwLock<Option<Player>>>>>,
    supervisor: LavalinkSupervisor,
}

impl PlayerManager {
    pub fn new(guild: GuildId, bots: Arc<BotMap>, supervisor: LavalinkSupervisor) -> Self {
        let player = Arc::new(
            bots.ids()
                .iter()
                .map(|id| (*id, Arc::new(RwLock::new(None))))
                .collect(),
        );

        let (player_states_sender, player_states_receiver) =
            tokio::sync::watch::channel(Vec::new());
        let player_states_sender = Arc::new(Mutex::new(player_states_sender));

        PlayerManager {
            guild,
            bots,
            player,
            supervisor,

            player_states_sender,
            player_states_receiver,
        }
    }

    pub async fn request(&self, request: PlayerRequest) -> Result<(), PlayerMapError> {
        let player = self.get_player_by_channel(request.get_channel()).await;
        let (bot, player) =
            player.ok_or_else(|| PlayerMapError::NoPlayerFound(request.get_channel()))?;
        let mut player_lock = player.write().await;
        let mut player = player_lock
            .deref_mut()
            .as_mut()
            .ok_or_else(|| PlayerMapError::NoPlayerFound(request.get_channel()))?;
        let lavalink = player.get_lavalink();

        let result = match request {
            PlayerRequest::Skip(_) => player.skip().await.map_err(PlayerMapError::PlayerError),
            PlayerRequest::BackSkip(_) => player
                .back_skip()
                .await
                .map_err(PlayerMapError::PlayerError),
            PlayerRequest::ClearQueue(_) => {
                player.clear_queue();
                return Ok(());
            }
            PlayerRequest::Playback(playback, _) => {
                player.playback(playback);
                return Ok(());
            }
            PlayerRequest::PauseResume(_) => player
                .dynamic_pause_resume()
                .await
                .map_err(PlayerMapError::PlayerError),
            PlayerRequest::Enqueue(track, _) => player
                .enqueue(track)
                .await
                .map_err(PlayerMapError::PlayerError),
        };
        drop(player_lock);
        self.handle_result(result, bot, lavalink).await
    }

    pub async fn search(
        &self,
        channel: ChannelId,
        query: String,
    ) -> Result<(UserId, Vec<Track>), PlayerMapError> {
        let (bot, player) = self
            .get_player_by_channel(channel)
            .await
            .ok_or(PlayerMapError::NoPlayerFound(channel))?;
        let (send, rev) = tokio::sync::oneshot::channel();
        let player_lock = player.read().await;
        let player = player_lock
            .as_ref()
            .ok_or(PlayerMapError::NoPlayerFound(channel))?;
        player.search(query, |res| async { if send.send(res).is_ok() {} });
        let lavalink = player.get_lavalink();
        drop(player_lock);

        match rev.await {
            Ok(res) => {
                self.handle_result(
                    res.map_err(PlayerMapError::PlayerError)
                        .map(|tracks| (bot, tracks)),
                    bot,
                    lavalink,
                )
                .await
            }
            Err(rec_err) => Err(PlayerMapError::SearchSenderDropped(rec_err)),
        }
    }

    async fn handle_result<T>(
        &self,
        error: Result<T, PlayerMapError>,
        bot: UserId,
        lavalink: LavalinkClient,
    ) -> Result<T, PlayerMapError> {
        if let Err(PlayerMapError::PlayerError(player_error)) = &error {
            if player_error.is_fatal() {
                if let Some(player) = self.player.get(&bot) {
                    if let Some(player) = player.write().await.take() {
                        player.disconnect().await.ok();
                    }
                };
                self.supervisor.request_new(bot, lavalink).await;
            }
        }
        error
    }

    pub async fn join(&self, channel: ChannelId) -> Result<(), PlayerMapError> {
        let mut bot_vec = self.player.deref().clone().drain().collect::<Vec<_>>();
        bot_vec.shuffle(rand::thread_rng().borrow_mut());

        for (bot, player) in bot_vec {
            if player.read().await.is_none() {
                let result = self.add_player(bot, channel).await;
                if result.is_ok() {
                    return result;
                }
            }
        }

        Err(PlayerMapError::NoFreeBot())
    }

    async fn add_player(&self, bot: UserId, channel: ChannelId) -> Result<(), PlayerMapError> {
        let player = self
            .player
            .get(&bot)
            .ok_or(PlayerMapError::NoBotWithID(bot, self.guild))?;
        let mut lock = player.write().await;
        if lock.is_some() {
            return Err(PlayerMapError::PlayerAlreadyExists(bot));
        }
        let (cache, songbird) = self
            .bots
            .get_bot_cache_songbird(&bot)
            .ok_or(PlayerMapError::NoBotWithID(bot, self.guild))?;
        cache
            .guild_field(self.guild, |_| ())
            .await
            .ok_or(PlayerMapError::NoBotWithID(bot, self.guild))?;
        let lavalink = self
            .supervisor
            .request_current(bot)
            .await
            .ok_or(PlayerMapError::NoLavalink(bot))?;
        let result = Player::new(bot, channel, self.guild, songbird, lavalink.clone())
            .await
            .map_err(PlayerMapError::PlayerError);
        let (player, rec) = self.handle_result(result, bot, lavalink).await?;
        *lock = Some(player);
        let state_lock = self.player_states_sender.lock().await;
        let mut states = (*(state_lock.borrow())).clone();
        states.push(rec);
        state_lock
            .send(states)
            .expect("PlayerStates Receiver dropped");

        Ok(())
    }

    pub async fn leave(&self, channel: ChannelId) -> Result<(), PlayerMapError> {
        let player = self.get_player_by_channel(channel).await;
        let (bot, player) = player.ok_or(PlayerMapError::NoPlayerFound(channel))?;
        let mut player_lock = player.write().await;
        player_lock
            .take()
            .ok_or(PlayerMapError::NoPlayerFound(channel))?
            .disconnect()
            .await
            .ok();
        let state_lock = self.player_states_sender.lock().await;
        let states: Vec<_> = (*(*state_lock.borrow()))
            .iter()
            .cloned()
            .filter(|s| !s.borrow().bot.eq(&bot))
            .collect();
        state_lock.send(states).ok();
        Ok(())
    }

    pub async fn get_all_player_states(&self) -> WatchReceiver<PlayerStates> {
        self.player_states_receiver.clone()
    }

    async fn get_player_by_channel(
        &self,
        channel: ChannelId,
    ) -> Option<(UserId, Arc<RwLock<Option<Player>>>)> {
        for (bot, player_lock) in self.player.iter() {
            if let Some(player) = player_lock.read().await.as_ref() {
                if channel == player.get_channel() {
                    return Some((*bot, player_lock.clone()));
                }
            }
        }
        None
    }

    async fn get_player_by_lavalink(
        &self,
        lavalink: &LavalinkClient,
    ) -> Option<(UserId, Arc<RwLock<Option<Player>>>)> {
        for (bot, player_lock) in self.player.iter() {
            if let Some(player) = player_lock.read().await.as_ref() {
                if Arc::ptr_eq(&lavalink.inner, &player.get_lavalink().inner) {
                    return Some((*bot, player_lock.clone()));
                }
            }
        }
        None
    }

    pub async fn handle_player_event(&self, event: LavalinkEvent) -> Result<(), ()> {
        let (bot, player) = self
            .get_player_by_lavalink(event.get_client())
            .await
            .ok_or(())?;
        let mut player_lock = player.write().await;
        let player = player_lock.as_mut().ok_or(())?;
        let lavalink = player.get_lavalink();
        let result = match event {
            LavalinkEvent::PlayerUpdate(update, _) => {
                player.update(update);
                Ok(())
            }
            LavalinkEvent::TrackStart(start, _) => {
                player.track_start(start);
                Ok(())
            }
            LavalinkEvent::TrackFinish(finish, _) => player
                .track_end(finish)
                .await
                .map_err(PlayerMapError::PlayerError),
        };
        self.handle_result(result, bot, lavalink)
            .await
            .map_err(|_| ())
    }
}

pub enum PlayerRequest {
    //Join(ChannelId),
    //Leave(ChannelId),
    Skip(ChannelId),
    BackSkip(ChannelId),
    ClearQueue(ChannelId),
    Playback(Playback, ChannelId),
    PauseResume(ChannelId),
    Enqueue(Track, ChannelId),
}

impl PlayerRequest {
    pub fn get_channel(&self) -> ChannelId {
        match self {
            //PlayerRequest::Join(channel) => *channel,
            //PlayerRequest::Leave(channel) => *channel,
            PlayerRequest::Skip(channel) => *channel,
            PlayerRequest::BackSkip(channel) => *channel,
            PlayerRequest::ClearQueue(channel) => *channel,
            PlayerRequest::Playback(_, channel) => *channel,
            PlayerRequest::PauseResume(channel) => *channel,
            PlayerRequest::Enqueue(_, channel) => *channel,
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
    #[error("Could not find Bot for ID: {0:?} in Guild: {1:?}")]
    NoBotWithID(UserId, GuildId),
    #[error("Player already exists for Bot with ID: {0:?}")]
    PlayerAlreadyExists(UserId),
    #[error("No Lavalink for Bot with ID: {0:?}")]
    NoLavalink(UserId),
}

impl PlayerMapError {
    pub fn is_fatal(&self) -> bool {
        if let PlayerMapError::PlayerError(error) = self {
            return error.is_fatal();
        }
        false
    }
}
