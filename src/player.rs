use std::borrow::BorrowMut;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arraydeque::{ArrayDeque, CapacityError};
use futures::Future;
use lavalink_rs::error::LavalinkError;
use lavalink_rs::model::{PlayerUpdate, Track, TrackFinish, TrackStart};
use lavalink_rs::LavalinkClient;
use serenity::model::prelude::{ChannelId, GuildId, UserId};
use songbird::error::JoinError;
use songbird::Songbird;
use thiserror::Error;
use tokio::sync::Notify;

const MUSIC_QUEUE_LIMIT: usize = 100;

pub struct Player {
    channel: ChannelId,
    guild: GuildId,
    lavalink: LavalinkClient,
    songbird: Arc<Songbird>,
    player_state: PlayerState,

    change: Arc<Notify>,
}

impl Player {
    pub async fn new(
        bot: UserId,
        channel: ChannelId,
        guild: GuildId,
        songbird: Arc<Songbird>,
        lavalink: LavalinkClient,
    ) -> Result<(Player, PlayerState), PlayerError> {
        let connection_info = songbird
            .join_gateway(guild, channel)
            .await
            .1
            .map_err(PlayerError::SongbirdJoin)?;
        lavalink
            .create_session(&connection_info)
            .await
            .map_err(PlayerError::Lavalink)?;

        let player = Player {
            channel,
            guild,
            lavalink,
            songbird,
            player_state: PlayerState::new(bot),

            change: Arc::new(Notify::new()),
        };
        let player_state = player.player_state.clone();

        Ok((player, player_state))
    }

    pub fn get_notify(&self) -> Arc<Notify> {
        self.change.clone()
    }

    pub fn get_lavalink(&self) -> LavalinkClient {
        self.lavalink.clone()
    }

    pub fn get_channel(&self) -> ChannelId {
        self.channel
    }

    pub fn get_bot(&self) -> UserId {
        self.player_state.bot
    }

    pub async fn resume(&mut self) -> Result<PlayerState, PlayerError> {
        self.lavalink
            .resume(self.guild)
            .await
            .map_err(PlayerError::Lavalink)?;
        self.player_state.play_state = PlayState::Play;

        self.change.notify_waiters();
        Ok(self.player_state.clone())
    }

    pub async fn pause(&mut self) -> Result<PlayerState, PlayerError> {
        self.lavalink
            .pause(self.guild)
            .await
            .map_err(PlayerError::Lavalink)?;
        self.player_state.play_state = PlayState::Pause;

        self.change.notify_waiters();
        Ok(self.player_state.clone())
    }

    pub async fn dynamic_pause_resume(&mut self) -> Result<PlayerState, PlayerError> {
        match self.player_state.play_state {
            PlayState::Play => self.pause().await,
            PlayState::Pause => self.resume().await,
        }
    }

    pub async fn skip(&mut self) -> Result<Option<PlayerState>, PlayerError> {
        let mut return_state = None;
        //If loop is one, move the current track to history, so a new Track gets played
        if let Playback::OneLoop = self.player_state.playback {
            if let Some((_, track)) = self.player_state.current.take() {
                if self.player_state.history.is_full() {
                    self.player_state
                        .history
                        .pop_back()
                        .expect("History is empty");
                }
                self.player_state
                    .history
                    .push_front(track)
                    .expect("History is full");
                self.change.notify_waiters();
                return_state = Some(self.player_state.clone());
            }
        }

        self.lavalink
            .stop(self.guild)
            .await
            .map_err(PlayerError::Lavalink)
            .map(|_| return_state)
    }

    pub async fn back_skip(&mut self) -> Result<Option<PlayerState>, PlayerError> {
        let mut changed = false;

        if let Some((_, track)) = self.player_state.current.take() {
            if self.player_state.playlist.is_full() {
                self.player_state
                    .playlist
                    .pop_back()
                    .expect("Playlist is empty");
            }
            self.player_state
                .playlist
                .push_front(track)
                .expect("Playlist is full");
            changed = true;
        }

        if let Some(history_track) = self.player_state.history.pop_back() {
            if self.player_state.playlist.is_full() {
                self.player_state
                    .playlist
                    .pop_back()
                    .expect("Playlist is empty");
            }
            self.player_state
                .playlist
                .push_front(history_track)
                .expect("Playlist is full");
            changed = true;
        }

        let mut return_state = None;
        if changed {
            self.change.notify_waiters();
            return_state = Some(self.player_state.clone())
        }
        self.lavalink
            .stop(self.guild)
            .await
            .map_err(PlayerError::Lavalink)
            .map(|_| return_state)
    }

    pub async fn enqueue(&mut self, track: Track) -> Result<PlayerState, PlayerError> {
        self.player_state
            .playlist
            .push_back(track)
            .map_err(PlayerError::PlaylistFull)?;

        if self.player_state.current.is_none() {
            self.play_next().await?;
        } else {
            self.change.notify_waiters();
        }
        Ok(self.player_state.clone())
    }

    pub fn clear_queue(&mut self) -> Option<PlayerState> {
        if !self.player_state.playlist.is_empty() {
            self.player_state.playlist.clear();
            self.change.notify_waiters();
            return Some(self.player_state.clone());
        }
        None
    }

    pub fn playback(&mut self, playback: Playback) -> PlayerState {
        if self.player_state.playback != playback {
            self.player_state.playback = playback;
        } else {
            self.player_state.playback = Playback::Normal;
        }
        self.change.notify_waiters();
        self.player_state.clone()
    }

    pub async fn disconnect(self) -> Result<(), PlayerError> {
        self.songbird
            .get(self.guild)
            .ok_or(PlayerError::NotInAVoiceChannel())?;
        self.songbird
            .remove(self.guild)
            .await
            .map_err(PlayerError::SongbirdLeave)?;
        self.lavalink
            .destroy(self.guild)
            .await
            .map_err(PlayerError::Lavalink)?;

        Ok(())
    }

    pub fn search<F, Fut>(&self, query: String, callback: F)
    where
        F: Send + Sync + 'static + FnOnce(Result<Vec<Track>, PlayerError>) -> Fut,
        Fut: Future<Output = ()> + Send + Sync,
    {
        let lavalink = self.lavalink.clone();
        tokio::spawn(async move {
            match lavalink.search_tracks(query).await {
                Err(e) => callback(Err(PlayerError::Lavalink(e))).await,
                Ok(tracks) => {
                    if tracks.load_type.eq("LOAD_FAILED") {
                        callback(Err(PlayerError::SearchFailed(tracks.load_type))).await;
                        return;
                    }
                    callback(Ok(tracks.tracks)).await;
                }
            }
        });
    }

    async fn play_next(&mut self) -> Result<(), PlayerError> {
        let mut changed = false;

        match self.player_state.playback {
            //Add Current to History
            Playback::Normal => {
                if let Some((_, track)) = self.player_state.current.take() {
                    if self.player_state.history.is_full() {
                        self.player_state
                            .history
                            .pop_back()
                            .expect("History is empty");
                    }
                    self.player_state
                        .history
                        .push_front(track)
                        .expect("History is full");
                    changed = true;
                }
            }
            //Add Current to Playlist
            Playback::AllLoop => {
                if let Some((_, track)) = self.player_state.current.take() {
                    if self.player_state.playlist.is_full() {
                        self.player_state
                            .playlist
                            .pop_back()
                            .expect("Playlist is empty");
                    }
                    self.player_state
                        .playlist
                        .push_back(track)
                        .expect("Playlist is full");
                    changed = true;
                }
            }
            //Reset Duration of Current
            Playback::OneLoop => {
                if let Some((duration, _)) = self.player_state.current.borrow_mut() {
                    *duration = Duration::from_secs(0);
                }
            }
        }

        //If current is None: Pull new one from Playlist
        if self.player_state.current.is_none() {
            if let Some(track) = self.player_state.playlist.pop_front() {
                self.player_state.current = Some((Duration::from_secs(0), track));
                self.player_state.play_state = PlayState::Play;
                changed = true;
            }
        }

        if changed {
            self.change.notify_waiters();
        }

        //Start if Current is some. Stop is Current is none.
        match self.player_state.current.take() {
            None => self
                .lavalink
                .stop(self.guild)
                .await
                .map_err(PlayerError::Lavalink),
            Some((_, track)) => self
                .lavalink
                .play(self.guild, track)
                .start()
                .await
                .map_err(PlayerError::Lavalink),
        }
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

        if let Some((pos, _)) = self.player_state.current.borrow_mut() {
            if pos.as_secs() != new_pos.as_secs() {
                *pos = new_pos;

                self.change.notify_waiters();
            }
        }
    }

    pub async fn track_end(&mut self, _end: TrackFinish) -> Result<(), PlayerError> {
        self.play_next().await
    }

    pub fn track_start(&mut self, _start: TrackStart) {
        //TODO maybe do something, maybe dont
    }
}

#[derive(Copy, Clone)]
pub enum PlayState {
    Play,
    Pause,
}

#[derive(Eq, PartialEq, Clone, Copy)]
pub enum Playback {
    Normal,
    AllLoop,
    OneLoop,
}

#[derive(Error, Debug)]
pub enum PlayerError {
    #[error("LavalinkError occurred: {0:?}")]
    Lavalink(LavalinkError),
    #[error("Error joining Channel: {0:?}")]
    SongbirdJoin(JoinError),
    #[error("Error leaving Channel: {0:?}")]
    SongbirdLeave(JoinError),
    #[error("Not in a Voice Channel")]
    NotInAVoiceChannel(),
    #[error("Playlist is full: {0:?}")]
    PlaylistFull(CapacityError<Track>),
    #[error("Search failed: {0:?}")]
    SearchFailed(String),
}

impl PlayerError {
    pub fn is_lavalink_error(&self) -> bool {
        matches!(self, PlayerError::Lavalink(_))
    }

    pub fn is_fatal(&self) -> bool {
        if let PlayerError::Lavalink(LavalinkError::ErrorWebsocketPayload(
            tokio_tungstenite::tungstenite::Error::ConnectionClosed,
        )) = self
        {
            return true;
        }
        false
    }
}

#[derive(Clone)]
pub struct PlayerState {
    pub bot: UserId,
    pub current: Option<(Duration, Track)>,
    pub playlist: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    pub history: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    pub play_state: PlayState,
    pub playback: Playback,
}

impl PlayerState {
    fn new(bot: UserId) -> Self {
        PlayerState {
            bot,
            current: None,
            playlist: ArrayDeque::new(),
            history: ArrayDeque::new(),
            play_state: PlayState::Play,
            playback: Playback::Normal,
        }
    }
}
