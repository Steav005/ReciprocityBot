#![allow(dead_code)]

use crate::voice_handler::VoiceHandlerError;
use arraydeque::ArrayDeque;
use async_compat::CompatExt;
use futures::prelude::future::Either;
use lavalink_rs::error::LavalinkError;
use lavalink_rs::model::{PlayerUpdate, Track, TrackFinish, TrackStart, UserId};
use lavalink_rs::LavalinkClient;
use log::{debug, error, info, warn};
use serenity::model::prelude::{ChannelId, GuildId};
use smol::channel::Sender;
use songbird::Songbird;
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;

const MUSIC_QUEUE_LIMIT: usize = 100;

//TODO Debug Format information
#[derive(Error, Debug)]
pub enum PlayerError {
    #[error("Error joining channel: {0:?}, Bot: {1}")]
    JoinError(songbird::error::JoinError, UserId),
    #[error("Error leaving channel: {0:?}, Bot: {1}")]
    LeaveError(songbird::error::JoinError, UserId),
    #[error("Error with Lavalink: {0:?}, Bot: {1}")]
    LavalinkError(LavalinkError, UserId),
    #[error("Bot is not in a Voice Channel, Bot: {0}")]
    NotInAVoiceChannel(UserId),
    #[error("Player does not exist, {0:?}, Bot: {1}")]
    PlayerDoesNotExist(GuildId, UserId),
}

pub enum Playback {
    Normal,
    SingleLoop,
    AllLoop,
}

pub struct PlayerStatus {}

pub struct Player {
    bot_id: UserId,
    guild_id: GuildId,
    channel_id: ChannelId,
    playlist: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    history: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    playback: Playback,
    lavalink: LavalinkClient,
    songbird: Arc<Songbird>,
}

impl Player {
    pub async fn new(
        bot_id: UserId,
        guild_id: GuildId,
        channel_id: ChannelId,
        lavalink: LavalinkClient,
        songbird: &Arc<Songbird>,
    ) -> Result<Player, PlayerError> {
        let (_, handler) = songbird.join_gateway(guild_id, channel_id).compat().await;

        match handler {
            Ok(connection_info) => match lavalink.create_session(&connection_info).await {
                Ok(_) => Ok(Player {
                    bot_id,
                    guild_id,
                    channel_id,
                    playlist: ArrayDeque::new(),
                    history: ArrayDeque::new(),
                    playback: Playback::Normal,
                    lavalink,
                    songbird: songbird.clone(),
                }),
                Err(err) => Err(PlayerError::LavalinkError(err, bot_id)),
            },
            Err(e) => Err(PlayerError::JoinError(e, bot_id)),
        }
    }

    pub async fn play(&self) -> Result<(), PlayerError> {
        todo!()
    }

    pub async fn stop(&self) -> Result<(), PlayerError> {
        todo!()
    }

    pub async fn skip(&self) -> Result<(), PlayerError> {
        todo!()
    }

    pub async fn reverse(&self) -> Result<(), PlayerError> {
        todo!()
    }

    pub async fn enqueue(&self) -> Result<(), PlayerError> {
        todo!()
    }

    pub fn playback(&mut self, playback: Playback) {
        self.playback = playback;
    }

    pub async fn disconnect(self) -> Result<(), PlayerError> {
        let guild = songbird::id::GuildId(self.guild_id.0);
        if self.songbird.get(guild).is_some() {
            if let Err(e) = self.songbird.remove(guild).compat().await {
                let msg = Err(PlayerError::LeaveError(e, self.bot_id));
                warn!("{:?}", msg);
                return msg;
            }

            if let Err(e) = self.lavalink.destroy(self.guild_id).compat().await {
                let msg = Err(PlayerError::LavalinkError(e, self.bot_id));
                error!("{:?}", msg);
                return msg;
            };
        } else {
            let msg = Err(PlayerError::NotInAVoiceChannel(self.bot_id));
            warn!("{:?}", msg);
            return msg;
        }

        Ok(())
    }

    pub async fn search(
        &self,
        query: String,
        callback: Sender<Result<Vec<Track>, Either<VoiceHandlerError, PlayerError>>>,
    ) -> Result<(), PlayerError> {
        let result = self.lavalink.auto_search_tracks(query).compat().await;

        todo!()
    }

    pub fn update(&mut self, update: PlayerUpdate) {
        todo!()
    }

    pub fn track_end(&mut self, end: TrackFinish) {
        todo!()
    }

    pub fn track_start(&mut self, end: TrackStart) {
        todo!()
    }
}
