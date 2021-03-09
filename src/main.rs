use crate::config::Config;
use crate::event_handler::EventHandler;
use crate::guild::{ReciprocityGuild, ReciprocityGuildError};
use crate::lavalink_handler::LavalinkHandler;
use crate::lavalink_supervisor::LavalinkSupervisor;
use crate::scheduler::SchedulerError;
use futures::Future;
use serenity::model::id::{GuildId, UserId};
use serenity::prelude::SerenityError;
use serenity::Client;
use songbird::{SerenityInit, SongbirdKey};
use std::collections::HashMap;
use std::ops::DerefMut;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::oneshot::Sender as OneShotSender;
use tokio::sync::Mutex;

mod config;
mod event_handler;
mod guild;
mod lavalink_handler;
mod lavalink_supervisor;
mod player;
mod player_manager;
mod scheduler;
mod task_handle;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    //TODO maybe use env for file name
    //Get Config
    let file_name: String = String::from("example_config.yml");
    let config = Config::new(file_name)?;

    //Build threaded RunTime
    let threaded_rt = tokio::runtime::Runtime::new()?;

    //Async main block
    threaded_rt.block_on::<Pin<Box<dyn Future<Output = Result<(), ReciprocityError>>>>>(
        Box::pin(async {
            //Make Oneshot Channel for occurring errors
            let (send_error, rec_error) = tokio::sync::oneshot::channel::<ReciprocityError>();
            let send_error = Arc::new(Mutex::new(Some(send_error)));

            //Build EventHandler and Bots using the EventHandler
            let event_handler = EventHandler::new();
            let bots = build_bots(config.bots.values(), event_handler.clone()).await?;

            //Build LavalinkEventHandler and LavalinkSupervisor using the EventHandler
            let lavalink_event_handler = LavalinkHandler::new();
            let lavalink_supervisor = LavalinkSupervisor::new(
                bots.iter().map(|(id, _)| *id).collect(),
                lavalink_event_handler.clone(),
                config.lavalink.clone(),
            )
            .await;

            //Make Hashmap of bots with Key: BotID, Value: (Http, Songbird)
            //Http is for actions by the respective bot and songbird is for voice related stuff
            let mut http_bots = HashMap::new();
            for (bot_id, client) in bots {
                http_bots.insert(
                    bot_id,
                    (
                        client.cache_and_http.http.clone(),
                        client
                            .data
                            .read()
                            .await
                            .get::<SongbirdKey>()
                            .ok_or(ReciprocityError::Songbird(bot_id))?
                            .clone(),
                    ),
                );
            }

            //Build every Guild
            for guild in config.guilds.values() {
                let id = GuildId(guild.guild_id);
                //Init Guild
                let (r_guild, run) = ReciprocityGuild::new(
                    id,
                    http_bots.clone(),
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

                //Spawn Task for Guild with error send for occurring errors
                let send_error = send_error.clone();
                tokio::spawn(async move {
                    let error = ReciprocityError::Scheduler(run.await, id);
                    try_send_error(send_error, error).await;
                });
            }

            //Await Error and match it
            match rec_error.await {
                Ok(err) => Err(err),
                Err(rec_err) => Err(ReciprocityError::DroppedReceive(rec_err)),
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
    #[error("Scheduler Error occurred: {0:?}, {1:?}")]
    Scheduler(SchedulerError, GuildId),
    #[error("Guild Error occurred: {0:?}, {1:?}")]
    Guild(ReciprocityGuildError, GuildId),
    #[error("ReceiveError was dropped: {0:?}")]
    DroppedReceive(RecvError),
}

///Tries sending an error, might fail if an error was already send before
async fn try_send_error(
    send_error: Arc<Mutex<Option<OneShotSender<ReciprocityError>>>>,
    error: ReciprocityError,
) {
    let mut swapped_send_error: Option<OneShotSender<ReciprocityError>> = None;
    {
        let mut send_error = send_error.lock_owned().await;
        std::mem::swap(send_error.deref_mut(), &mut swapped_send_error);
    }
    if let Some(send_error) = swapped_send_error {
        if send_error.send(error).is_ok() {
            //Ignore
        };
    }
}

///Builds bots from token and with EventHandler
async fn build_bots(
    bot_token: impl Iterator<Item = &String>,
    event_handler: EventHandler,
) -> Result<Vec<(UserId, Client)>, ReciprocityError> {
    let mut bots = Vec::new();
    for token in bot_token {
        let client = Client::builder(token.clone())
            .register_songbird()
            .event_handler(event_handler.clone())
            .await
            .map_err(ReciprocityError::Serenity)?;
        let id = client
            .cache_and_http
            .http
            .get_current_application_info()
            .await
            .map_err(ReciprocityError::Serenity)?
            .id;
        bots.push((id, client));
    }

    Ok(bots)
}
