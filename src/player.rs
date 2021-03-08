#![allow(dead_code)]

use crate::voice_handler::{Notify, PlayerStatusSwap, VoiceHandlerError};
use arc_swap::Guard;
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
    #[error("Playlist is full, {0:?}, Bot: {1}")]
    PlaylistFull(GuildId, UserId),
}

#[derive(Clone, Eq, PartialEq)]
pub enum Playback {
    Normal,
    SingleLoop,
    AllLoop,
}

#[derive(Clone, Eq, PartialEq)]
pub enum PlayState {
    Play,
    Pause,
}

#[derive(Clone)]
pub struct PlayerStatus {
    bot_id: UserId,
    guild_id: GuildId,
    channel_id: ChannelId,
    current: Option<(Duration, Track)>,
    playlist: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    history: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    play_state: PlayState,
    playback: Playback,
}

pub struct Player {
    bot_id: UserId,
    guild_id: GuildId,
    channel_id: ChannelId,
    change: Notify,
    status: PlayerStatusSwap,
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
        status_change: Notify,
        status_swap: PlayerStatusSwap,
    ) -> Result<Player, PlayerError> {
        let (_, handler) = songbird.join_gateway(guild_id, channel_id).compat().await;

        let status = Arc::new(Some(PlayerStatus {
            bot_id,
            guild_id,
            channel_id,
            current: None,
            playlist: ArrayDeque::new(),
            history: ArrayDeque::new(),
            play_state: PlayState::Play,
            playback: Playback::Normal,
        }));
        status_swap.store(status);
        status_change.notify(usize::MAX);

        match handler {
            Ok(connection_info) => match lavalink.create_session(&connection_info).await {
                Ok(_) => Ok(Player {
                    bot_id,
                    guild_id,
                    channel_id,
                    change: status_change,
                    status: status_swap,
                    lavalink,
                    songbird: songbird.clone(),
                }),
                Err(err) => Err(PlayerError::LavalinkError(err, bot_id)),
            },
            Err(e) => Err(PlayerError::JoinError(e, bot_id)),
        }
    }

    pub async fn resume(&self) -> Result<(), PlayerError> {
        if let Err(error) = self.lavalink.resume(self.guild_id).compat().await {
            return Err(PlayerError::LavalinkError(error, self.bot_id));
        }
        self.set_play_state(PlayState::Play);

        Ok(())
    }

    pub async fn pause(&self) -> Result<(), PlayerError> {
        if let Err(error) = self.lavalink.pause(self.guild_id).compat().await {
            return Err(PlayerError::LavalinkError(error, self.bot_id));
        }
        self.set_play_state(PlayState::Pause);

        Ok(())
    }

    pub async fn dynamic_pause_resume(&self) -> Result<(), PlayerError> {
        if let Some(play_state) = (**self.status.load())
            .clone()
            .map(|s| Some(s.play_state))
            .unwrap_or(None)
        {
            match play_state {
                PlayState::Play => self.pause().await?,
                PlayState::Pause => self.resume().await?,
            }
        }
        Ok(())
    }

    fn set_play_state(&self, state: PlayState) {
        if let Some(mut status) = (**self.status.load()).clone() {
            if status.play_state != state {
                status.play_state = state;
                self.status.store(Arc::new(Some(status)));
                self.change.notify(usize::MAX);
            }
        }
    }

    pub async fn skip(&self) {
        if self.lavalink.skip(self.guild_id).compat().await.is_some() {
            //Ignore
        }
    }

    pub async fn back_skip(&self) {
        if let Some(mut status) = (**self.status.load()).clone() {
            if let Some((_, track)) = status.current {
                if status.playlist.is_full() {
                    status.playlist.pop_back().expect("Playlist is empty");
                }
                status.playlist.push_front(track).expect("Playlist is full");
                status.current = None;
            }

            if let Some(history_track) = status.history.pop_front() {
                if status.playlist.is_full() {
                    status.playlist.pop_back().expect("History is empty");
                }
                status
                    .playlist
                    .push_front(history_track)
                    .expect("History is full");
            }

            self.status.store(Arc::new(Some(status)));
            self.change.notify(usize::MAX);
        }

        if self.lavalink.skip(self.guild_id).compat().await.is_some() {
            //Ignore
        }
    }

    pub async fn enqueue(&self, track: Track) -> Result<(), PlayerError> {
        if let Some(mut status) = (**self.status.load()).clone() {
            if status.playlist.push_back(track).is_err() {
                return Err(PlayerError::PlaylistFull(self.guild_id, self.bot_id));
            }
            let current = status.current.clone();
            self.status.store(Arc::new(Some(status)));

            if current.is_none() {
                self.play_next().await?;
            } else {
                self.change.notify(usize::MAX);
            }
        }

        Ok(())
    }

    pub fn clear_queue(&self){
        if let Some(mut status) = (**self.status.load()).clone(){
            if !status.playlist.is_empty(){
                status.playlist.clear();
                self.status.store(Arc::new(Some(status)));
                self.change.notify(usize::MAX);
            }
        }
    }

    pub fn playback(&self, playback: Playback) {
        if let Some(mut status) = (**self.status.load()).clone() {
            if status.playback != playback {
                status.playback = playback;
            } else {
                status.playback = Playback::Normal;
            }
            self.status.store(Arc::new(Some(status)));
            self.change.notify(usize::MAX);
        }
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

    pub fn search(
        &self,
        query: String,
        callback: Sender<Result<Vec<Track>, Either<VoiceHandlerError, PlayerError>>>,
    ) {
        let bot_id = self.bot_id;
        let lavalink = self.lavalink.clone();
        std::thread::spawn(move || {
            smol::future::block_on(async move {
                let result = lavalink.auto_search_tracks(query).compat().await;
                let send_res = match result {
                    Ok(mut tracks) => callback.send(Ok(tracks.tracks.drain(..10).collect())).await,
                    Err(error) => {
                        callback
                            .send(Err(Either::Right(PlayerError::LavalinkError(
                                error, bot_id,
                            ))))
                            .await
                    }
                };
                if send_res.is_err() {
                    //Ignore
                }
            })
        });
    }

    async fn play_next(&self) -> Result<(), PlayerError> {
        let mut change = false;
        if let Some(mut status) = (**self.status.load()).clone() {
            if let Some((_, track)) = status.current {
                if status.history.is_full() {
                    status.history.pop_back().expect("History is empty");
                }
                status.history.push_front(track).expect("History is full");
                change = true;
            }
            status.current = None;

            if let Some(track) = status.playlist.pop_front() {
                status.current = Some((Duration::from_secs(0), track));
                status.play_state = PlayState::Play;
                change = true;
            }

            let current = status.current.clone();

            if change {
                self.status.store(Arc::new(Some(status)));
                self.change.notify(usize::MAX);
            }

            if let Some((_, track)) = current {
                if let Err(err) = self
                    .lavalink
                    .play(self.guild_id, track)
                    .start()
                    .compat()
                    .await
                {
                    return Err(PlayerError::LavalinkError(err, self.bot_id));
                }
            }
        }
        Ok(())
    }

    pub fn update(&mut self, update: PlayerUpdate) {
        let update_time = Duration::from_secs(update.state.time as u64);
        let now = Duration::from_secs(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("SystemTime before UNIX")
                .as_secs(),
        );
        let elapsed = now - update_time;
        let new_pos = Duration::from_millis(update.state.position as u64) + elapsed;

        if let Some(mut status) = (**self.status.load()).clone() {
            if let Some((pos, _)) = status.current.as_mut() {
                if pos.as_secs() != new_pos.as_secs() {
                    *pos = new_pos;

                    self.status.store(Arc::new(Some(status)));
                    self.change.notify(usize::MAX);
                }
            }
        }
    }

    pub async fn track_end(&mut self, end: TrackFinish) {
        if let Err(err) = self.play_next().await {
            warn!("{:?}", err);
        }
    }

    pub fn track_start(&mut self, _start: TrackStart) {
        //TODO maybe do something, maybe dont
    }
}
