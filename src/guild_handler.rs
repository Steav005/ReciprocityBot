#![allow(dead_code)]

use crate::scheduler::TaskScheduler;
use futures::prelude::*;
use serenity::http::Http;
use serenity::model::id::{ChannelId, GuildId};
use std::pin::Pin;

pub struct ReciprocityGuild<'a> {
    guild: GuildId,
    channel: ChannelId,
    scheduler: TaskScheduler,
    bots: Vec<&'a Http>,
    // Something for Events
    // Something for Messages
    // Something for VoiceHandling
}

impl<'a> ReciprocityGuild<'a> {
    pub fn new(
        guild: GuildId,
        channel: ChannelId,
        bots: Vec<&'a Http>,
    ) -> (Self, Pin<Box<dyn Future<Output = ()> + 'a>>) {
        let (scheduler, _runner) = TaskScheduler::new(guild, channel, bots.clone());

        let _guild = ReciprocityGuild {
            guild,
            channel,
            scheduler,

            bots,
            //More
        };

        unimplemented!();
    }
}
