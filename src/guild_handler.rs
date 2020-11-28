#![allow(dead_code)]

use serenity::model::id::{ChannelId, GuildId};
use crate::scheduler::TaskScheduler;
use serenity::http::Http;
use std::pin::Pin;
use futures::prelude::*;

pub struct ReciprocityGuild<'a>{
    guild: GuildId,
    channel: ChannelId,
    scheduler: TaskScheduler,
    bots: Vec<&'a Http>,
    // Something for Events
    // Something for Messages
    // Something for VoiceHandling
}


impl<'a> ReciprocityGuild<'a>{
    pub fn new(guild: GuildId, channel: ChannelId, bots: Vec<&'a Http>,
    ) -> (Self, Pin<Box<dyn Future<Output = ()> + 'a>>){
        let (scheduler, _runner) = TaskScheduler::new(guild, channel, bots.clone());

        let _guild = ReciprocityGuild{
            guild,
            channel,
            scheduler,

            bots,
            //More
        };

        unimplemented!();
    }
}