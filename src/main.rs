use crate::config::{Config, GuildConfig};
use crate::event_handler::{Event, EventHandler};
use crate::guild_handler::{GuildHandlerError, ReciprocityGuild};
use crate::scheduler::TaskScheduler;
use crate::voice_handler::{VoiceHandler, VoiceHandlerError};
use async_compat::{Compat, CompatExt};
use easy_parallel::Parallel;
use futures::future::Either;
use futures::FutureExt;
use lavalink_rs::model::UserId as LavalinkUser;
use serenity::client::Extras;
use serenity::http::{CacheHttp, Http};
use serenity::model::id::{ChannelId, GuildId};
use serenity::model::prelude::UserId;
use serenity::prelude::SerenityError;
use serenity::Client;
use smol::channel::Receiver;
use smol::prelude::Future;
use smol::Executor;
use songbird::{SerenityInit, SongbirdKey};
use std::collections::VecDeque;
use std::sync::Arc;
use thiserror::Error;

mod config;
mod event_handler;
mod event_handler_old;
mod guild_handler;
mod message_manager;
mod player;
mod scheduler;
mod structs;
mod voice_handler;

fn main() -> Result<(), ReciprocityError> {
    println!("Hello, world!");
    let file_name: String = String::from("example_config.yml");
    let config = Config::new(file_name).map_err(ReciprocityError::StdError)?;

    let ex = Executor::new();
    smol::future::block_on(ex.run(async {
        //Error channel
        let (send_stop_request, receive_stop_request) = smol::channel::bounded::<()>(1);
        let (send_error_occurred, receive_error_occurred) = smol::channel::bounded::<()>(1);

        let e_guilds = config
            .guilds
            .values()
            .map(|guild| GuildId(guild.guild_id))
            .collect();
        let e_guilds = e_guilds;
        //Build EventHandler
        let (event_handler, mut event_receiver) = EventHandler::new(e_guilds);

        //Get Bots: Vector aus (UserID, Client)
        let mut bot_clients = build_bots(config.bots.values(), event_handler).await?;
        //make other bot list
        let bots: Vec<(UserId, Arc<Http>)> = bot_clients
            .iter()
            .map(|(id, client)| (*id, client.cache_and_http.http.clone()))
            .collect();

        //Make bot list for use in Voice Handler: Vec(UserID, Arc<Http>, Arc<Songbird>)
        let mut voice_handler_bots = Vec::new();
        for (id, client) in bot_clients.iter() {
            voice_handler_bots.push((
                lavalink_rs::model::UserId(id.0),
                client.cache_and_http.http.clone(),
                client
                    .data
                    .read()
                    .compat()
                    .await
                    .get::<SongbirdKey>()
                    .cloned()
                    .ok_or_else(|| {
                        ReciprocityError::GenericError(String::from("Error getting Songbird"))
                    })?,
            ));
        }
        let (voice_handler, voice_handler_run) =
            VoiceHandler::new(voice_handler_bots, config.clone());
        let voice_receive_stop = receive_stop_request.clone();
        let voice_send_error = send_error_occurred.clone();

        //Build list for guild threads
        let mut guilds: Vec<(GuildId, ChannelId, Receiver<Event>)> = event_receiver
            .drain(..)
            .map(|(id, rec)| {
                config
                    .guilds
                    .iter()
                    .find(|(_, cfg)| cfg.guild_id == id.0)
                    .map(|(_, config)| {
                        (GuildId(config.guild_id), ChannelId(config.channel_id), rec)
                    })
            })
            .filter(|g| g.is_some())
            .map(|guild| guild.unwrap())
            .collect();

        let mut result = Parallel::new()
            .add(|| {
                smol::future::block_on(ex.run(async move {
                    let result = futures::future::select(
                        voice_handler_run.boxed(),
                        voice_receive_stop.recv().boxed(),
                    )
                    .await;
                    if let Either::Left((error, _)) = result {
                        if let Ok(()) = voice_send_error.send(()).await {
                            //Ignore
                        }
                        Err(ReciprocityError::VoiceHandlerError(error))
                    } else {
                        Ok(())
                    }
                }))
            })
            .each(
                bot_clients.drain(..).zip(
                    (0..bots.len())
                        .map(|_| (send_error_occurred.clone(), receive_stop_request.clone())),
                ),
                |((id, mut client), (send_error, stop))| {
                    smol::future::block_on(ex.run(async move {
                        let result = futures::future::select(
                            client.start_autosharded().compat().boxed(),
                            stop.recv().boxed(),
                        )
                        .await;
                        if let Either::Left((error, _)) = result {
                            if let Ok(()) = send_error.send(()).await {
                                //Ignore
                            }
                            Err(match error {
                                Ok(_) => ReciprocityError::ClientStoppedUnexpectedly(),
                                Err(e) => ReciprocityError::SerenityError(e),
                            })
                        } else {
                            Ok(())
                        }
                    }))
                },
            )
            //.each(guilds.drain(..), |(guild, channel, rec)| {
            //    smol::future::block_on(ex.run(async move {
            //        ReciprocityGuild::new(guild, channel, bots.clone(), voice_handler.clone(), config)
            //    }))
            //})
            .finish(|| {
                smol::future::block_on(async {
                    if let Err(e) = receive_error_occurred.recv().await {
                        panic!(e);
                    }
                    drop(send_stop_request)
                })
            });
        let result = result
            .0
            .drain(..)
            .find(|r| r.is_err())
            .expect("Exit without error");
        result
    }))
}

#[derive(Debug, Error)]
pub enum ReciprocityError {
    #[error("Serenity Error occurred: {0:?}")]
    SerenityError(SerenityError),
    #[error("Serenity Client stopped unexpectedly")]
    ClientStoppedUnexpectedly(),
    #[error("{0:?}")]
    GenericError(String),
    #[error("{0:?}")]
    StdError(String),
    #[error("Voice Handler Error occurred: {0:?}")]
    VoiceHandlerError(VoiceHandlerError),
    #[error("Guild Handler Error occurred: {0:?}")]
    GuildHandlerError(GuildHandlerError),
}

async fn build_bots(
    bot_token: impl Iterator<Item = &String>,
    event_handler: EventHandler,
) -> Result<Vec<(UserId, Client)>, ReciprocityError> {
    let mut bots = Vec::new();
    for token in bot_token {
        let client = Client::builder(token.clone())
            .register_songbird()
            .event_handler(event_handler.clone())
            .compat()
            .await
            .map_err(ReciprocityError::SerenityError)?;
        let id = client
            .cache_and_http
            .http
            .get_current_application_info()
            .compat()
            .await
            .map_err(ReciprocityError::SerenityError)?
            .id;
        bots.push((id, client));
    }

    Ok(bots)
}
