#![allow(dead_code)]

use lavalink_rs::LavalinkClient;
use serenity::http::Http;

pub(crate) struct ReciprocityBot<'a> {
    pub http: &'a Http,
    pub lavalink: LavalinkClient,
    //TODO Check if needed
}
