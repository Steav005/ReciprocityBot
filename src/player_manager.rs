use crate::guild::ReciprocityGuildError::PlayerMap;
use crate::lavalink_handler::{GuildLavalinkHandler, LavalinkEvent};
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::player::{Playback, Player, PlayerError};
use lavalink_rs::model::Track;
use lavalink_rs::LavalinkClient;
use log::{error, warn};
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::prelude::{ChannelId, UserId};
use songbird::Songbird;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::{RwLock, RwLockWriteGuard};

#[derive(Clone)]
pub struct PlayerManager {
    bots: HashMap<UserId, (Arc<Http>, Arc<Songbird>)>,
    supervisor: LavalinkSupervisor,
    player_map: HashMap<UserId, Arc<RwLock<Option<Player>>>>,
}

impl PlayerManager {
    pub fn new(
        bots: HashMap<UserId, (Arc<Http>, Arc<Songbird>)>,
        supervisor: LavalinkSupervisor,
    ) -> PlayerManager {
        let player_map: HashMap<_, _> = bots
            .keys()
            .map(|id| (*id, Arc::new(RwLock::new(None))))
            .collect();

        PlayerManager {
            bots,
            supervisor,
            player_map,
        }
    }

    pub async fn request(&self, request: PlayerRequest) -> Result<(), PlayerMapError> {
        match request {
            PlayerRequest::Join(_) => {}
            PlayerRequest::Leave(_) => {}
            PlayerRequest::Skip(_) => {}
            PlayerRequest::BackSkip(_) => {}
            PlayerRequest::ClearQueue(_) => {}
            PlayerRequest::Playback(_, _) => {}
            PlayerRequest::PauseResume(_) => {}
        }
        todo!()
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

    async fn free_bots(&self) -> Vec<UserId> {
        //Problem: nicht garantiert, dass alle Bots in der Gilde present sind
        todo!()
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
}
