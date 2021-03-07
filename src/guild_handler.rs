#![allow(dead_code)]

use crate::config::Config;
use crate::event_handler::Event;
use crate::message_manager::{MessageManager, MessageManagerError};
use crate::scheduler::{SchedulerError, TaskScheduler};
use crate::voice_handler::{VoiceHandler, VoiceHandlerError};
use futures::future::{select_all, FutureExt};
use futures::prelude::*;
use serenity::http::Http;
use serenity::model::id::{ChannelId, GuildId, UserId};
use smol::channel::Receiver;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

pub struct ReciprocityGuild {
    guild: GuildId,
    scheduler: TaskScheduler,
    bots: Vec<(UserId, Arc<Http>)>,
    voice_handler: VoiceHandler,
    config: Config,
}

#[derive(Debug, Error)]
pub enum GuildHandlerError {
    #[error("{0:?}")]
    SchedulerError(SchedulerError),
    #[error("{0:?}")]
    MessageManagerError(MessageManagerError),
    #[error("{0:?}")]
    VoiceHandlerError(VoiceHandlerError),
}

impl From<SchedulerError> for GuildHandlerError {
    fn from(e: SchedulerError) -> Self {
        GuildHandlerError::SchedulerError(e)
    }
}

impl From<MessageManagerError> for GuildHandlerError {
    fn from(e: MessageManagerError) -> Self {
        GuildHandlerError::MessageManagerError(e)
    }
}

impl From<VoiceHandlerError> for GuildHandlerError {
    fn from(e: VoiceHandlerError) -> Self {
        GuildHandlerError::VoiceHandlerError(e)
    }
}

type ReciprocityGuildRunner = Pin<Box<dyn Future<Output = GuildHandlerError> + Send>>;

impl ReciprocityGuild {
    pub fn new(
        guild: GuildId,
        channel: ChannelId,
        bots: Vec<(UserId, Arc<Http>)>,
        voice_handler: VoiceHandler,
        event_receiver: Receiver<Event>,
        config: Config,
    ) -> Result<(Self, ReciprocityGuildRunner), GuildHandlerError> {
        let mut run_all: Vec<Pin<Box<dyn Future<Output = GuildHandlerError> + Send>>> = Vec::new();

        let (scheduler, run) = TaskScheduler::new(
            guild,
            channel,
            bots.clone().drain(..).map(|(_, http)| http).collect(),
        );
        run_all.push(run.map_into::<GuildHandlerError>().boxed());

        let guild = ReciprocityGuild {
            guild,
            scheduler,

            bots,
            voice_handler,
            config,
        };

        Ok((guild, select_all(run_all).map(|(res, _, _)| res).boxed()))
    }
}
