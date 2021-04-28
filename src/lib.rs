#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use log::info;
use serenity::model::id::{GuildId, UserId};
use serenity::prelude::SerenityError;
use thiserror::Error;
use tokio::task::{JoinError, JoinHandle};

use crate::bots::{BotError, BotMap};
use crate::config::Config;
use crate::event_handler::EventHandler;
use crate::guild::{ReciprocityGuild, ReciprocityGuildError};
use crate::lavalink_handler::LavalinkHandler;
use lavalink_rs::error::LavalinkError;
use lavalink_rs::LavalinkClient;
use crate::net::CompanionCommunicationHandler;

mod bots;
pub mod config;
mod context;
mod event_handler;
pub mod guild;
mod lavalink_handler;
mod multi_key_map;
mod player;
mod task_handle;
mod net;

pub struct ReciprocityBot {}

impl ReciprocityBot {
    pub async fn run(config: Arc<Config>) -> Result<(), ReciprocityError> {
        //Build EventHandler and Bots using the EventHandler
        let event_handler = EventHandler::default();
        info!("Starting {} Bots", config.bots.len());
        let (bots, join_handles) = start_bots(config.bots.values(), event_handler.clone()).await?;

        //Build LavalinkEventHandler and LavalinkSupervisor using the EventHandler
        let lavalink_event_handler = LavalinkHandler::new();
        let mut lavalink: HashMap<UserId, LavalinkClient> = HashMap::new();
        info!("Starting Lavalink for each Bot");
        for bot in bots.ids() {
            let client = LavalinkClient::builder(bot)
                .set_host(&config.lavalink.address)
                .set_password(&config.lavalink.password)
                //.set_is_ssl(true)
                .build(lavalink_event_handler.clone())
                .await
                .map_err(ReciprocityError::Lavalink)?;
            lavalink.insert(bot, client);
        }
        let lavalink = Arc::new(lavalink);
        let mut player_manager = HashMap::new();

        //Build every Guild
        info!("Starting {} Guilds", config.guilds.len());
        for guild in config.guilds.values() {
            let id = GuildId(guild.guild_id);
            //Init Guild
            let r_guild = ReciprocityGuild::new(
                id,
                bots.clone(),
                event_handler.clone(),
                lavalink.clone(),
                config.clone(),
            )
            .map_err(|e| ReciprocityError::Guild(e, id))?;
            player_manager.insert(r_guild.get_id(), r_guild.get_player_manager());

            //Add Guild to EventHandler and LavalinkEventHandler
            event_handler.add_guild(id, Arc::new(r_guild.clone())).await;
            lavalink_event_handler
                .add_guild(id, Arc::new(r_guild))
                .await;
        }

        if let Some(net_cfg) = config.net.clone(){
            info!("Starting Network Companion Handler");
            let _net_handler = CompanionCommunicationHandler::new(net_cfg, bots.clone(), Arc::new(player_manager));
        }

        info!("Started Everything");
        let (res, _, _) = futures::future::select_all(join_handles).await;
        match res {
            Ok(res) => res.map_err(ReciprocityError::Serenity),
            Err(err) => Err(ReciprocityError::JoinErrorClient(err)),
        }
    }
}

#[derive(Error, Debug)]
pub enum ReciprocityError {
    #[error("Serenity Error occurred: {0:?}")]
    Serenity(SerenityError),
    #[error("Songbird not in Client: {0:?}")]
    Songbird(UserId),
    #[error("Guild Error occurred: {0:?}, {1:?}")]
    Guild(ReciprocityGuildError, GuildId),
    #[error("Join Error occurred for Client future: {0:?}")]
    JoinErrorClient(JoinError),
    #[error("Error creating Bots: {0:?}")]
    BotCreateError(BotError),
    #[error("Lavalink Error occured: {0:?}")]
    Lavalink(LavalinkError),
}

///Builds and starts bots from token and with EventHandler
async fn start_bots(
    bot_token: impl Iterator<Item = &String>,
    event_handler: EventHandler,
) -> Result<(Arc<BotMap>, Vec<JoinHandle<Result<(), SerenityError>>>), ReciprocityError> {
    let mut bots = BotMap::new(event_handler);
    let mut join_handle = Vec::new();
    for token in bot_token {
        join_handle.push(
            bots.add_bot(token)
                .await
                .map_err(ReciprocityError::BotCreateError)?,
        );
    }

    Ok((Arc::new(bots), join_handle))
}
