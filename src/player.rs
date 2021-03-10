use arraydeque::{ArrayDeque, CapacityError};
use futures::Future;
use lavalink_rs::error::LavalinkError;
use lavalink_rs::model::{PlayerUpdate, Track, TrackFinish, TrackStart};
use lavalink_rs::LavalinkClient;
use serenity::model::prelude::{ChannelId, GuildId};
use songbird::error::JoinError;
use songbird::Songbird;
use std::borrow::BorrowMut;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::Notify;

const MUSIC_QUEUE_LIMIT: usize = 100;

pub struct Player {
    channel: ChannelId,
    guild: GuildId,
    lavalink: LavalinkClient,
    songbird: Arc<Songbird>,

    current: Option<(Duration, Track)>,
    playlist: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    history: ArrayDeque<[Track; MUSIC_QUEUE_LIMIT]>,
    play_state: PlayState,
    playback: Playback,

    change: Arc<Notify>,
}

impl Player {
    pub async fn new(
        channel: ChannelId,
        guild: GuildId,
        songbird: Arc<Songbird>,
        lavalink: LavalinkClient,
    ) -> Result<Player, PlayerError> {
        let connection_info = songbird
            .join_gateway(guild, channel)
            .await
            .1
            .map_err(PlayerError::SongbirdJoin)?;
        lavalink
            .create_session(&connection_info)
            .await
            .map_err(PlayerError::Lavalink)?;

        Ok(Player {
            channel,
            guild,
            lavalink,
            songbird,

            current: None,
            playlist: ArrayDeque::new(),
            history: ArrayDeque::new(),
            play_state: PlayState::Play,
            playback: Playback::Normal,

            change: Arc::new(Notify::new()),
        })
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

    pub async fn resume(&mut self) -> Result<(), PlayerError> {
        self.lavalink
            .resume(self.guild)
            .await
            .map_err(PlayerError::Lavalink)?;
        self.play_state = PlayState::Play;

        self.change.notify_waiters();
        Ok(())
    }

    pub async fn pause(&mut self) -> Result<(), PlayerError> {
        self.lavalink
            .pause(self.guild)
            .await
            .map_err(PlayerError::Lavalink)?;
        self.play_state = PlayState::Pause;

        self.change.notify_waiters();
        Ok(())
    }

    pub async fn dynamic_pause_resume(&mut self) -> Result<(), PlayerError> {
        match self.play_state {
            PlayState::Play => self.pause().await,
            PlayState::Pause => self.resume().await,
        }
    }

    pub async fn skip(&mut self) {
        //If loop is one, move the current track to history, so a new Track gets played
        if let Playback::OneLoop = self.playback {
            if let Some((_, track)) = self.current.take() {
                if self.history.is_full() {
                    self.history.pop_back().expect("History is empty");
                }
                self.history.push_front(track).expect("History is full");
                self.change.notify_waiters();
            }
        }

        if self.lavalink.skip(self.guild).await.is_some() {
            //Ignore
        }
    }

    pub async fn back_skip(&mut self) {
        let mut changed = false;

        if let Some((_, track)) = self.current.take() {
            if self.playlist.is_full() {
                self.playlist.pop_back().expect("Playlist is empty");
            }
            self.playlist.push_front(track).expect("Playlist is full");
            changed = true;
        }

        if let Some(history_track) = self.history.pop_back() {
            if self.playlist.is_full() {
                self.playlist.pop_back().expect("Playlist is empty");
            }
            self.playlist
                .push_front(history_track)
                .expect("Playlist is full");
            changed = true;
        }

        if changed {
            self.change.notify_waiters();
        }
        self.skip().await;
    }

    pub async fn enqueue(&mut self, track: Track) -> Result<(), PlayerError> {
        self.playlist
            .push_back(track)
            .map_err(PlayerError::PlaylistFull)?;

        if self.current.is_none() {
            self.play_next().await?;
        } else {
            self.change.notify_waiters();
        }
        Ok(())
    }

    pub fn clear_queue(&mut self) {
        if !self.playlist.is_empty() {
            self.playlist.clear();
            self.change.notify_waiters();
        }
    }

    pub fn playback(&mut self, playback: Playback) {
        if self.playback != playback {
            self.playback = playback;
        } else {
            self.playback = Playback::Normal;
        }
        self.change.notify_waiters();
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

    pub async fn play_next(&mut self) -> Result<(), PlayerError> {
        let mut changed = false;

        match self.playback {
            //Add Current to History
            Playback::Normal => {
                if let Some((_, track)) = self.current.take() {
                    if self.history.is_full() {
                        self.history.pop_back().expect("History is empty");
                    }
                    self.history.push_front(track).expect("History is full");
                    changed = true;
                }
            }
            //Add Current to Playlist
            Playback::AllLoop => {
                if let Some((_, track)) = self.current.take() {
                    if self.playlist.is_full() {
                        self.playlist.pop_back().expect("Playlist is empty");
                    }
                    self.playlist.push_back(track).expect("Playlist is full");
                    changed = true;
                }
            }
            //Reset Duration of Current
            Playback::OneLoop => {
                if let Some((duration, _)) = self.current.borrow_mut() {
                    *duration = Duration::from_secs(0);
                }
            }
        }

        //If current is None: Pull new one from Playlist
        if self.current.is_none() {
            if let Some(track) = self.playlist.pop_front() {
                self.current = Some((Duration::from_secs(0), track));
                self.play_state = PlayState::Play;
                changed = true;
            }
        }

        if changed {
            self.change.notify_waiters();
        }

        //Start if Current is some. Stop is Current is none.
        match self.current.take() {
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

        if let Some((pos, _)) = self.current.borrow_mut() {
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

pub enum PlayState {
    Play,
    Pause,
}

#[derive(Eq, PartialEq)]
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
}
