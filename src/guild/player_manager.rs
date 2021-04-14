use crate::bots::BotMap;
use crate::guild::message_manager::EmoteAction;
use crate::guild::ReciprocityGuild;
use crate::lavalink_handler::LavalinkEvent;
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::multi_key_map::{HashArc, TripleHashMap};
use crate::player::{Playback, Player, PlayerError, PlayerState};
use arc_swap::ArcSwap;
use lavalink_rs::model::Track;
use lavalink_rs::{LavalinkClient, LavalinkClientInner};
use log::error;
use rand::prelude::SliceRandom;
use serenity::async_trait;
use serenity::model::prelude::{ChannelId, GuildId, UserId};
use std::borrow::BorrowMut;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::watch::{Receiver as WatchReceiver, Receiver, Sender as WatchSender};
use tokio::sync::{Mutex, Notify, RwLock};

pub type PlayerStates = Vec<WatchReceiver<Arc<PlayerState>>>;
pub type PlayerMapType = TripleHashMap<
    UserId,
    HashArc<Mutex<LavalinkClientInner>>,
    ChannelId,
    Arc<RwLock<Option<Player>>>,
>;

#[derive(Clone)]
pub struct PlayerManager {
    guild: GuildId,
    bots: Arc<BotMap>,
    player_states: Arc<RwLock<PlayerStates>>,
    player: Arc<RwLock<PlayerMapType>>,
    supervisor: LavalinkSupervisor,
}

impl PlayerManager {
    pub fn new(guild: GuildId, bots: Arc<BotMap>, supervisor: LavalinkSupervisor) -> Self {
        let mut player = TripleHashMap::new();
        for bot in bots.ids() {
            player.insert(bot, Arc::new(RwLock::new(None)))
        }
        let player = Arc::new(RwLock::new(player));
        let player_states = Arc::new(RwLock::new(Vec::new()));

        PlayerManager {
            guild,
            bots,
            player,
            supervisor,
            player_states,
        }
    }

    pub async fn request(&self, request: PlayerRequest) -> Result<(), PlayerMapError> {
        let player = self
            .player
            .read()
            .await
            .get_k2(&request.get_channel())
            .map(|(bot, player)| (*bot, player.clone()));
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
        match result {
            Err(e) => Err(self.handle_error(e, bot, lavalink).await),
            Ok(_) => Ok(()),
        }
    }

    pub async fn search(
        &self,
        channel: ChannelId,
        query: String,
    ) -> Result<(UserId, Vec<Track>), PlayerMapError> {
        let (bot, player) = self
            .player
            .read()
            .await
            .get_k2(&channel)
            .map(|(bot, player)| (*bot, player.clone()))
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
                let res = res
                    .map_err(PlayerMapError::PlayerError)
                    .map(|tracks| (bot, tracks));
                match res {
                    Err(e) => Err(self.handle_error(e, bot, lavalink).await),
                    Ok(vec) => Ok(vec),
                }
            }
            Err(rec_err) => Err(PlayerMapError::SearchSenderDropped(rec_err)),
        }
    }

    async fn handle_error(
        &self,
        error: PlayerMapError,
        bot: UserId,
        lavalink: LavalinkClient,
    ) -> PlayerMapError {
        if let PlayerMapError::PlayerError(player_error) = &error {
            if player_error.is_fatal() {
                if let Some(player) = self.player.read().await.get(&bot).cloned() {
                    if let Some(player) = player.write().await.take() {
                        player.disconnect().await.ok();
                        self.player.write().await.sub_k1_k2(&bot);
                    }
                };
                self.supervisor.request_new(bot, lavalink).await;
            }
        }
        error
    }

    pub async fn join(&self, channel: ChannelId) -> Result<(), PlayerMapError> {
        let mut bot_vec = self
            .player
            .read()
            .await
            .iter()
            .map(|(bot, player)| (*bot, player.clone()))
            .collect::<Vec<_>>();
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
        let mut map_lock = self.player.write().await;
        let player = map_lock
            .get(&bot)
            .cloned()
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

        let (player, rec) = match result {
            Err(e) => {
                drop(map_lock);
                drop(lock);
                return Err(self.handle_error(e, bot, lavalink).await);
            },
            Ok((player, rec)) => (player, rec),
        };

        *lock = Some(player);
        let mut states = self.player_states.write().await;
        states.push(rec);
        map_lock.add_k1_k2(bot, HashArc::from(lavalink.inner), channel);

        Ok(())
    }

    pub async fn leave(&self, channel: ChannelId) -> Result<(), PlayerMapError> {
        //Get bot and player while removing channel and lavalink form the HashMap
        let (bot, player) = {
            let mut lock = self.player.write().await;
            let (bot, player) = lock
                .get_k2(&channel)
                .map(|(bot, player)| (*bot, player.clone()))
                .ok_or(PlayerMapError::NoPlayerFound(channel))?;
            lock.sub_k1_k2(&bot);
            (bot, player)
        };

        let mut player_lock = player.write().await;
        player_lock
            .take()
            .ok_or(PlayerMapError::NoPlayerFound(channel))?
            .disconnect()
            .await
            .ok();
        let mut states = self.player_states.write().await;
        *states = states
            .drain(..)
            .filter(|s| !s.borrow().bot.eq(&bot))
            .collect();
        Ok(())
    }

    pub async fn get_all_player_states(&self) -> PlayerStates {
        self.player_states.read().await.clone()
    }

    pub async fn handle_player_event(
        &self,
        event: LavalinkEvent,
        client: LavalinkClient,
    ) -> Result<(), ()> {
        let (bot, player) = self
            .player
            .read()
            .await
            .get_k1(&HashArc::from(client.inner))
            .map(|(bot, player)| (*bot, player.clone()))
            .ok_or(())?;
        let mut player_lock = player.write().await;
        let player = player_lock.as_mut().ok_or(())?;
        let lavalink = player.get_lavalink();
        let result = match event {
            LavalinkEvent::Update(update) => {
                player.update(update);
                Ok(())
            }
            LavalinkEvent::Start(start) => {
                player.track_start(start);
                Ok(())
            }
            LavalinkEvent::Finish(finish) => player
                .track_end(finish)
                .await
                .map_err(PlayerMapError::PlayerError),
        };

        if let Err(e) = result {
            self.handle_error(e, bot, lavalink).await;
            Err(())
        } else {
            Ok(())
        }
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
