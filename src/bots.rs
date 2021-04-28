use crate::event_handler::EventHandler;
use serenity::cache::Cache;
use serenity::http::Http;
use serenity::model::prelude::{ChannelId, GuildId, UserId, VoiceState};
use serenity::prelude::SerenityError;
use serenity::{CacheAndHttp, Client};
use songbird::{SerenityInit, Songbird, SongbirdKey};
use std::ops::{Deref, Not};
use std::sync::Arc;
use thiserror::Error;
use tokio::task::JoinHandle;

pub struct BotMap {
    bots: Vec<Arc<Bot>>,
    event_handler: EventHandler,
}

impl BotMap {
    pub fn new(event_handler: EventHandler) -> Self {
        BotMap {
            bots: Vec::new(),
            event_handler,
        }
    }

    pub async fn add_bot(
        &mut self,
        token: &str,
    ) -> Result<JoinHandle<Result<(), SerenityError>>, BotError> {
        let (bot, handle) = Bot::new(token, self.event_handler.clone()).await?;

        if self.contains_id(&bot.id) {
            return Err(BotError::DuplicateBotEntry(bot.id));
        }

        self.bots.push(Arc::new(bot));
        Ok(handle)
    }

    pub async fn get_user_voice_state(&self, user: &UserId, guild: &GuildId) -> Option<VoiceState> {
        for bot in &self.bots {
            if let Some(voice_states) = bot
                .cache
                .guild_field(guild, |g| g.voice_states.clone())
                .await
            {
                return voice_states.get(user).cloned();
            }
        }
        None
    }

    pub async fn get_any_user_voice_channel(&self, user: &UserId) -> Option<(GuildId, ChannelId)> {
        //TODO anders lösen?
        for bot in &self.bots {
            let guilds = bot.cache.guilds().await;
            for guild in guilds.iter() {
                if let Some(voice_states) = bot
                    .cache
                    .guild_field(guild, |g| g.voice_states.clone())
                    .await
                {
                    if let Some(vs) = voice_states.get(user) {
                        if let Some(ch) = vs.channel_id {
                            return Some((*guild, ch));
                        }
                    }
                }
            }
        }
        return None;
    }

    ///Returns whether non-bot users are in a channel or not
    pub async fn user_in_channel(&self, channel: &ChannelId, guild: &GuildId) -> bool {
        for bot in &self.bots {
            if let Some(voice_states) = bot
                .cache
                .guild_field(guild, |g| g.voice_states.clone())
                .await
            {
                for (user, state) in voice_states {
                    if state.channel_id.unwrap_or(ChannelId(0)).eq(channel).not() {
                        continue;
                    }
                    if let Some(user) = bot.cache.user(user).await {
                        if !user.bot {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    ///Returns whether non-bot users are in a channel or not using a specific bot
    pub async fn user_in_channel_with_bot(
        &self,
        channel: &ChannelId,
        guild: &GuildId,
        bot: UserId,
    ) -> Option<bool> {
        let bot = self.get_bot_by_id(bot)?;
        if let Some((voice_states, members)) = bot
            .cache
            .guild_field(guild, |g| (g.voice_states.clone(), g.members.clone()))
            .await
        {
            for (user, state) in voice_states {
                if state.channel_id.unwrap_or(ChannelId(0)).eq(channel).not() {
                    continue;
                }
                if let Some(member) = members.get(&user) {
                    if !member.user.bot {
                        return Some(true);
                    }
                }
            }
        }
        Some(false)
    }

    pub fn contains_id(&self, bot: &UserId) -> bool {
        self.bots.iter().any(|b| b.id.eq(bot))
    }

    pub fn get_bot_by_id(&self, id: UserId) -> Option<Arc<Bot>> {
        self.bots.iter().find(|b| id.eq(&b.deref().id)).cloned()
    }

    pub fn bots(&self) -> Vec<Arc<Bot>> {
        self.bots.clone()
    }

    pub fn get_bot_cache_songbird(&self, id: &UserId) -> Option<(Arc<Cache>, Arc<Songbird>)> {
        self.bots
            .iter()
            .find(|b| b.id.eq(id))
            .map(|b| (b.cache.clone(), b.songbird.clone()))
    }

    pub fn ids(&self) -> Vec<UserId> {
        self.bots.iter().map(|b| b.id).collect()
    }

    pub async fn get_any_guild_bot(&self, guild: &GuildId) -> Option<Arc<Bot>> {
        for bot in &self.bots {
            if bot.cache.guild_field(guild, |_| ()).await.is_some() {
                return Some(bot.clone());
            }
        }
        None
    }
}

#[derive(Clone)]
pub struct Bot {
    id: UserId,
    cache: Arc<Cache>,
    http: Arc<Http>,
    cache_http: Arc<CacheAndHttp>,
    songbird: Arc<Songbird>,
}

impl Bot {
    pub async fn new(
        token: &str,
        event_handler: EventHandler,
    ) -> Result<(Self, JoinHandle<Result<(), SerenityError>>), BotError> {
        let id = Http::new_with_token(token)
            .get_current_application_info()
            .await
            .map_err(BotError::Serenity)?
            .id;
        let mut client = Client::builder(token)
            .register_songbird()
            .event_handler(event_handler.clone())
            .await
            .map_err(BotError::Serenity)?;
        let http = client.cache_and_http.http.clone();
        let cache = client.cache_and_http.cache.clone();
        let cache_http = client.cache_and_http.clone();
        let songbird = client
            .data
            .read()
            .await
            .get::<SongbirdKey>()
            .ok_or(BotError::Songbird(id))?
            .clone();

        let bot = Bot {
            id,
            cache,
            http,
            cache_http,
            songbird,
        };

        Ok((
            bot,
            tokio::spawn(async move { client.start_autosharded().await }),
        ))
    }

    pub fn id(&self) -> UserId {
        self.id
    }

    pub fn cache(&self) -> &Arc<Cache> {
        &self.cache
    }

    pub fn http(&self) -> &Arc<Http> {
        &self.http
    }

    pub fn cache_http(&self) -> &Arc<CacheAndHttp> {
        &self.cache_http
    }

    pub fn songbird(&self) -> &Arc<Songbird> {
        &self.songbird
    }
}

#[derive(Error, Debug)]
pub enum BotError {
    #[error("Bot already exists in Map: {0:?}")]
    DuplicateBotEntry(UserId),
    #[error("Serenity Error occurred: {0:?}")]
    Serenity(SerenityError),
    #[error("Songbird not in Client: {0:?}")]
    Songbird(UserId),
}
