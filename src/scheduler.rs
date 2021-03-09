#![allow(dead_code)]

use crate::task_handle::{Routes, TaskHandle};
use futures::prelude::*;
use log::{error, warn};
use serenity::http::routing::Route;
use serenity::http::Http;
use serenity::model::id::{ChannelId, GuildId};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use strum::IntoEnumIterator;
use thiserror::Error;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;

const QUEUE_LIMIT: usize = 1000;
const GUILD_CHECK_SLEEP: Duration = Duration::from_secs(60);

///Schedules Tasks within a Guild
#[derive(Clone)]
pub struct TaskScheduler {
    task_sender: Sender<TaskHandle>,
    guild_id: GuildId,
    channel_id: ChannelId,
}

impl TaskScheduler {
    ///Builds a new Scheduler for a Guild based on the used Https
    pub fn new(
        guild_id: GuildId,
        channel_id: ChannelId,
        worker: Vec<Arc<Http>>,
    ) -> (Self, Pin<Box<dyn Future<Output = SchedulerError> + Send>>) {
        let (task_sender, task_receiver) = tokio::sync::mpsc::channel(QUEUE_LIMIT);

        let scheduler = TaskScheduler {
            task_sender,
            guild_id,
            channel_id,
        };

        (
            // The Scheduler used for enqueuing new Tasks
            scheduler,
            // Future which will run the Scheduler
            run_async(worker, guild_id, channel_id, task_receiver).boxed(),
        )
    }

    /// Get a Task Sender Clone for enqueuing tasks
    pub fn get_task_sender(&self) -> Sender<TaskHandle> {
        self.task_sender.clone()
    }
}

#[derive(Error, Debug)]
pub enum SchedulerError {
    #[error("Send was dropped, Guild: {0:?}")]
    Receive(GuildId),
    #[error("Guild: {0:?}, Route: {1:?}")]
    Send(GuildId, Routes),
    #[error("Couldn't get Rate Limit for {0:?}. Guild: {1:?}, Channel: {2:?}")]
    RateLimit(Route, GuildId, ChannelId),
    #[error("Couldn't get Reset Time for {0:?}. Guild: {1:?}, Channel: {2:?}")]
    ResetTime(Route, GuildId, ChannelId),
}

/// Receive Task and send it to the proper scheduler
async fn split_receive(
    mut receiver: Receiver<TaskHandle>,
    sender: HashMap<Routes, Sender<TaskHandle>>,
    guild_id: GuildId,
) -> SchedulerError {
    loop {
        let task = match receiver.recv().await {
            None => return SchedulerError::Receive(guild_id),
            Some(task) => task,
        };
        let route = task.get_route();
        let sender = match sender.get(&route) {
            None => {
                warn!("No Route Sender. {:?}, {:?}", guild_id, route);
                task.drop();
                continue;
            }
            Some(sender) => sender,
        };

        if let Err(err) = sender.try_send(task) {
            match err {
                TrySendError::Full(task) => {
                    warn!("Buffer Full. {:?}, Route: {:?}", guild_id, route);
                    task.drop();
                }
                TrySendError::Closed(_) => return SchedulerError::Send(guild_id, route),
            }
        }
    }
}

async fn run_async(
    worker: Vec<Arc<Http>>,
    guild_id: GuildId,
    channel_id: ChannelId,
    task_receiver: Receiver<TaskHandle>,
) -> SchedulerError {
    let mut pool: Vec<Pin<Box<dyn Future<Output = SchedulerError> + Send>>> = Vec::new();
    let mut route_senders: HashMap<Routes, Sender<TaskHandle>> = HashMap::new();
    let mut route_receivers: HashMap<Routes, Receiver<TaskHandle>> = HashMap::new();

    for route in Routes::iter() {
        let (s, r) = tokio::sync::mpsc::channel(QUEUE_LIMIT);
        route_senders.insert(route, s);
        route_receivers.insert(route, r);
    }

    //This one is for taking any received tasks and pushing it to the appropriate receiver
    pool.push(split_receive(task_receiver, route_senders, guild_id).boxed());

    //Here we start the receivers. They will schedule any received task to a free Http object
    for (route, rec) in route_receivers.drain() {
        pool.push(
            task_type_scheduler(
                rec,
                move |channel, guild| route.get_serenity_route(channel, guild),
                guild_id,
                channel_id,
                worker.clone(),
            )
            .boxed(),
        )
    }

    //Here we make them all run
    futures::future::select_all(pool).await.0
}

//Hosts all Https for this Task
async fn task_type_scheduler<F>(
    shared_receiver: Receiver<TaskHandle>,
    route: F,
    guild_id: GuildId,
    channel_id: ChannelId,
    worker: Vec<Arc<Http>>,
) -> SchedulerError
where
    F: Send + Clone + Fn(ChannelId, GuildId) -> Route,
{
    let mut pool: Vec<Pin<Box<dyn Future<Output = SchedulerError> + Send>>> = Vec::new();
    let shared_receiver = Arc::new(Mutex::new(shared_receiver));

    for w in worker.iter() {
        pool.push(Box::pin(task_http_loop(
            shared_receiver.clone(),
            route.clone(),
            guild_id,
            channel_id,
            w.clone(),
        )));
    }

    futures::future::select_all(pool).await.0
}

//Makes sure Http is ready for Task, then tries to get one, for executing it
async fn task_http_loop<F>(
    shared_receiver: Arc<Mutex<Receiver<TaskHandle>>>,
    route: F,
    guild_id: GuildId,
    channel_id: ChannelId,
    worker: Arc<Http>,
) -> SchedulerError
where
    F: Send + Fn(ChannelId, GuildId) -> Route,
{
    //TODO kinda ugly, maybe rethink scheduler design
    //Check if we are allowed to do stuff in this guild
    loop {
        if worker.get_guild(guild_id.0).await.is_ok() {
            //Else continue
            break;
        }
        //If we are not allowed, retry after some time
        tokio::time::sleep(GUILD_CHECK_SLEEP).await;
    }

    loop {
        let ready = {
            let routes_map = worker.ratelimiter.routes();
            let routes_map = routes_map.read().await;
            let target_route = route(channel_id, guild_id);
            let rate_limit = match routes_map.get(&target_route) {
                None => return SchedulerError::RateLimit(target_route, guild_id, channel_id),

                Some(rl) => rl,
            };
            let rate_limit = rate_limit.lock().await;
            if rate_limit.remaining() > 0 {
                Ok(())
            } else {
                Err(match rate_limit.reset_after() {
                    None => return SchedulerError::ResetTime(target_route, guild_id, channel_id),
                    Some(reset) => reset,
                })
            }
        };

        match ready {
            Ok(_) => {
                if shared_receiver
                    .lock()
                    .await
                    .recv()
                    .await
                    .expect("Task splitter was dropped")
                    .run(worker.clone())
                    .await
                    .is_err()
                {
                    //If there was an error, check if we are allowed to do stuff in this guild
                    loop {
                        if worker.get_guild(guild_id.0).await.is_ok() {
                            //Else continue
                            break;
                        }
                        //If we are not allowed, retry after some time
                        tokio::time::sleep(GUILD_CHECK_SLEEP).await;
                    }
                }
            }
            Err(duration) => {
                tokio::time::sleep(duration).await;
            }
        }
    }
}
