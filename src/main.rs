use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use serenity::http::Http;
use serenity::model::id::{GuildId, UserId};
use serenity::prelude::{SerenityError, TypeMapKey};
use serenity::{CacheAndHttp, Client};
use songbird::{SerenityInit, Songbird, SongbirdKey};
use thiserror::Error;
use tokio::task::{JoinError, JoinHandle};

use crate::config::Config;
use crate::event_handler::EventHandler;
use crate::guild::{ReciprocityGuild, ReciprocityGuildError};
use crate::lavalink_handler::LavalinkHandler;
use crate::lavalink_supervisor::LavalinkSupervisor;

mod config;
mod event_handler;
mod guild;
mod lavalink_handler;
mod lavalink_supervisor;
pub mod player;
pub mod task_handle;

pub type BotList = Arc<HashMap<UserId, (Arc<CacheAndHttp>, Arc<Songbird>)>>;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    //TODO maybe use env for file name
    //Get Config
    let file_name: String = String::from("example_config.yml");
    let config = Arc::new(Config::new(file_name)?);

    //Build threaded RunTime
    let threaded_rt = tokio::runtime::Runtime::new()?;

    //Async main block
    threaded_rt.block_on::<Pin<Box<dyn Future<Output = Result<(), ReciprocityError>>>>>(
        Box::pin(async {
            //Build EventHandler and Bots using the EventHandler
            let event_handler = EventHandler::new();
            let (bots, join_handles) =
                start_bots(config.bots.values(), event_handler.clone()).await?;
            let bots = Arc::new(bots);

            //Build LavalinkEventHandler and LavalinkSupervisor using the EventHandler
            let lavalink_event_handler = LavalinkHandler::new();
            let lavalink_supervisor = LavalinkSupervisor::new(
                bots.iter().map(|(id, _)| *id).collect(),
                lavalink_event_handler.clone(),
                config.clone(),
            )
            .await;

            //Build every Guild
            for guild in config.guilds.values() {
                let id = GuildId(guild.guild_id);
                //Init Guild
                let r_guild = ReciprocityGuild::new(
                    id,
                    bots.clone(),
                    event_handler.clone(),
                    lavalink_supervisor.clone(),
                    config.clone(),
                )
                .map_err(|e| ReciprocityError::Guild(e, id))?;

                //Add Guild to EventHandler and LavalinkEventHandler
                event_handler.add_guild(id, Arc::new(r_guild.clone())).await;
                lavalink_event_handler
                    .add_guild(id, Arc::new(r_guild))
                    .await;
            }

            let (res, _, _) = futures::future::select_all(join_handles).await;
            match res {
                Ok(res) => res.map_err(ReciprocityError::Serenity),
                Err(err) => Err(ReciprocityError::JoinErrorClient(err)),
            }
        }),
    )?;

    Ok(())
}

#[derive(Error, Debug)]
enum ReciprocityError {
    #[error("Serenity Error occurred: {0:?}")]
    Serenity(SerenityError),
    #[error("Songbird not in Client: {0:?}")]
    Songbird(UserId),
    #[error("Guild Error occurred: {0:?}, {1:?}")]
    Guild(ReciprocityGuildError, GuildId),
    #[error("Join Error occurred for Client future: {0:?}")]
    JoinErrorClient(JoinError),
}

pub struct BotUserId;
impl TypeMapKey for BotUserId {
    type Value = UserId;
}

///Builds and starts bots from token and with EventHandler
async fn start_bots(
    bot_token: impl Iterator<Item = &String>,
    event_handler: EventHandler,
) -> Result<
    (
        HashMap<UserId, (Arc<CacheAndHttp>, Arc<Songbird>)>,
        Vec<JoinHandle<Result<(), SerenityError>>>,
    ),
    ReciprocityError,
> {
    let mut bots = HashMap::new();
    let mut join_handle = Vec::new();
    for token in bot_token {
        let id = Http::new_with_token(token)
            .get_current_application_info()
            .await
            .map_err(ReciprocityError::Serenity)?
            .id;
        let mut client = Client::builder(token.clone())
            .register_songbird()
            .event_handler(event_handler.clone())
            .await
            .map_err(ReciprocityError::Serenity)?;
        let http = client.cache_and_http.clone();
        let songbird = client
            .data
            .read()
            .await
            .get::<SongbirdKey>()
            .ok_or(ReciprocityError::Songbird(id))?
            .clone();

        client.data.write().await.insert::<BotUserId>(id);
        join_handle.push(tokio::spawn(
            async move { client.start_autosharded().await },
        ));

        bots.insert(id, (http, songbird));
    }

    Ok((bots, join_handle))
}
