#![allow(dead_code)]

use crate::scheduler::TaskError::FailedExecution;
use arc_swap::ArcSwap;
use arrayvec::ArrayVec;
use async_compat::CompatExt;
use futures::prelude::*;
use log::{debug, error, info, warn};
use serde_json::Value as SerdeValue;
use serenity::http::routing::Route;
use serenity::http::Http;
use serenity::model::channel::ReactionType;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use serenity::prelude::SerenityError;
use smol::channel;
use smol::channel::{Receiver, RecvError, Sender, TrySendError};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

const QUEUE_LIMIT: usize = 1000;

pub struct Final<T> {
    t: T,
}

impl<T> Final<T> {
    fn new(t: T) -> Final<T> {
        Final { t }
    }
}

impl<T> Deref for Final<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.t
    }
}

///Any Error that can be returned by any "enqueued" Task
#[derive(Debug, Error)]
pub enum TaskError {
    #[error("Execution of the Task failed: Serenity Error: {0:?}")]
    FailedExecution(SerenityError),
    #[error("The requested Bot does not exists")]
    BotDoesNotExist(),
    #[error("The Task was dropped before execution")]
    Dropped(),
}

///The Tasks, that can be enqueued
#[derive(Debug)]
pub enum Task {
    DeleteMessageReaction(ChannelId, MessageId, UserId, ReactionType),
    DeleteMessage(ChannelId, MessageId),
    AddMessageReaction(ChannelId, MessageId, ReactionType),
    SendDM(ChannelId, String),
}

///All the Routes which will be used by any Task
pub const ROUTES: [fn(ChannelId, GuildId) -> Route; 3] = [
    channel_id_message_id_reaction,
    channel_id_message,
    channel_id_message_id_reaction_self,
];

const fn channel_id_message_id_reaction(channel: ChannelId, _: GuildId) -> Route {
    Route::ChannelsIdMessagesIdReactions(channel.0)
}

const fn channel_id_message(channel: ChannelId, _: GuildId) -> Route {
    Route::ChannelsIdMessages(channel.0)
}

const fn channel_id_message_id_reaction_self(channel: ChannelId, _: GuildId) -> Route {
    Route::ChannelsIdMessagesIdReactionsUserIdType(channel.0)
}

///Task Handler, which will handle the execution, errors and general Task information
pub struct TaskHandle {
    receiver: Receiver<Result<(), TaskError>>,
    sender: Sender<Result<(), TaskError>>,
    pub task: Final<Task>,
}

impl TaskHandle {
    ///Builds a TaskHandle from a Task
    pub fn new(task: Task) -> TaskHandle {
        let (sender, receiver) = channel::bounded(1);
        TaskHandle {
            receiver,
            sender,
            task: Final::new(task),
        }
    }

    ///Tries to complete the Task with the help of an Http Instance
    async fn run(self, client: Arc<Http>) {
        let result: Result<(), TaskError> = match self.task.deref() {
            //ALWAYS USE COMPAT
            Task::DeleteMessageReaction(channel, message, user, reaction) => client
                .delete_reaction(channel.0, message.0, Some(user.0), reaction)
                .compat()
                .await
                .map_err(FailedExecution),
            Task::DeleteMessage(channel, message) => client
                .delete_message(channel.0, message.0)
                .compat()
                .await
                .map_err(FailedExecution),
            Task::AddMessageReaction(channel, message, reaction) => client
                .create_reaction(channel.0, message.0, reaction)
                .compat()
                .await
                .map_err(FailedExecution),
            Task::SendDM(channel, message) => client
                .send_message(channel.0, &SerdeValue::String(message.clone()))
                .compat()
                .await
                .map(|_| ())
                .map_err(FailedExecution),
        };
        info!(
            "Run {:?}, with client(token): {:?}, {:?}",
            self.task.t, client.token, result
        );
        if let Err(err) = self.sender.try_send(result) {
            debug!("Task Error Result was dropped: {:?}", err.into_inner());
        }
    }

    ///Drops the Task/TaskHandler
    fn drop(self) {
        info!("Dropping Task: {:?}", self.task.t);
        if let Err(err) = self.sender.try_send(Err(TaskError::Dropped())) {
            debug!(
                "Task Error was dropped: {:?}",
                err.into_inner().unwrap_err()
            )
        }
    }

    fn drop_no_bot(self) {
        info!("Dropping {:?}, Reason: No Bot Exists", self.task.t);
        if let Err(err) = self.sender.try_send(Err(TaskError::BotDoesNotExist())) {
            debug!(
                "Task Error was dropped: {:?}",
                err.into_inner().unwrap_err()
            )
        }
    }

    /// Await Task completion
    pub async fn complete(&self) -> Result<(), TaskError> {
        self.receiver.recv().await.expect("Unexpectedly Dropped")
    }

    ///Returns the Route, the underlying Task requires
    pub fn get_route_id(&self) -> usize {
        let route = match self.task.t {
            Task::DeleteMessageReaction(_, _, _, _) => channel_id_message_id_reaction,
            Task::DeleteMessage(_, _) => channel_id_message,
            Task::AddMessageReaction(_, _, _) => channel_id_message_id_reaction_self,
            Task::SendDM(_, _) => channel_id_message,
        } as usize;

        ROUTES
            .iter()
            .position(|f| *f as usize == route)
            .expect("Route not Found")
    }
}

///Schedules Tasks within a Guild
pub struct TaskScheduler {
    task_sender: Sender<TaskHandle>,
    guild_id: Final<GuildId>,
    channel_id: ChannelId,
}

impl TaskScheduler {
    ///Builds a new Scheduler for a Guild based on the used Https
    pub fn new(
        guild_id: GuildId,
        channel_id: ChannelId,
        worker: Vec<Arc<Http>>,
    ) -> (Self, Pin<Box<dyn Future<Output = SchedulerError> + Send>>) {
        let (task_sender, task_receiver) = channel::bounded(QUEUE_LIMIT);

        let scheduler = TaskScheduler {
            task_sender,
            guild_id: Final::new(guild_id),
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

    // Changes the channel_id in case the Bot Channel Changed
    //pub fn change_channel_id(&self, channel_id: ChannelId) {
    //    debug!(
    //        "Changing Channel ID for {:?}, to {:?}",
    //        self.guild_id.t, channel_id
    //    );
    //    self.channel_id.swap(Arc::new(channel_id));
    //}

    /////Does not Block if the Buffer is full
    //pub fn try_send_task(&self, task: TaskHandle) -> Result<(), SendError> {
    //    info!("Try Sending {:?}, to {:?}", task.task.t, self.guild_id.t);
    //    if let Err(err) = self.task_sender.try_send(task) {
    //        match err {
    //            TrySendError::Full(task) => {
    //                warn!("Buffer Full. {:?}", self.guild_id.t);
    //                task.drop();
    //                return Err(SendError::BufferFull());
    //            }
    //            TrySendError::Closed(_) => {
    //                error!("Task Receiver dropped. {:?}", self.guild_id.t);
    //                return Err(SendError::StreamDropped(self.guild_id.t));
    //            }
    //        }
    //    }
    //    Ok(())
    //}
    //
    /////Blocks if the Buffer is full
    //pub async fn send_task(&self, task: TaskHandle) -> Result<(), SendError> {
    //    if self.task_sender.send(task).await.is_err() {
    //        error!("Task Receiver dropped. {:?}", self.guild_id.t);
    //        return Err(SendError::StreamDropped(self.guild_id.t));
    //    };
    //    Ok(())
    //}
}

//#[derive(Debug, Error)]
//pub enum SendError {
//    #[error("The Buffer is full")]
//    BufferFull(),
//    #[error("Task Receiver dropped. GuildID: {0:?}")]
//    StreamDropped(GuildId),
//}

#[derive(Error, Debug)]
pub enum SchedulerError {
    #[error("Guild: {0:?}, Error: {1:?}")]
    ReceiveError(GuildId, RecvError),
    #[error("Guild: {0:?}, RouteID: {1}")]
    SendError(GuildId, usize),
    #[error("Couldn't get Rate Limit for {0:?}. Guild: {1:?}, Channel: {2:?}")]
    RateLimitError(Route, GuildId, ChannelId),
    #[error("Couldn't get Reset Time for {0:?}. Guild: {1:?}, Channel: {2:?}")]
    ResetTimeError(Route, GuildId, ChannelId),
}

/// Receive Task and send it to the proper scheduler
async fn split_receive(
    receiver: Receiver<TaskHandle>,
    sender: [Sender<TaskHandle>; ROUTES.len()],
    guild_id: GuildId,
) -> SchedulerError {
    loop {
        let task = match receiver.recv().await {
            Err(err) => {
                let msg = SchedulerError::ReceiveError(guild_id, err);
                error!("{:?}", &msg);
                return msg;
            }
            Ok(task) => task,
        };
        let route_id = task.get_route_id();

        if let Err(err) = sender[route_id].try_send(task) {
            match err {
                TrySendError::Full(task) => {
                    warn!("Buffer Full. {:?}, RouteID: {:?}", guild_id, route_id);
                    task.drop();
                }
                TrySendError::Closed(_) => {
                    let msg = SchedulerError::SendError(guild_id, route_id);
                    error!("{:?}", &msg);
                    return msg;
                }
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
    let mut route_senders: ArrayVec<[Sender<TaskHandle>; ROUTES.len()]> = ArrayVec::new();
    let mut route_receivers: ArrayVec<[Receiver<TaskHandle>; ROUTES.len()]> = ArrayVec::new();

    for _ in 0..ROUTES.len() {
        let (s, r) = channel::bounded(QUEUE_LIMIT);
        route_senders.push(s);
        route_receivers.push(r);
    }

    //This one is for taking any received tasks and pushing it to the appropriate receiver
    let route_senders = route_senders.into_inner().unwrap();
    pool.push(split_receive(task_receiver, route_senders, guild_id).boxed());

    //Here we start the receivers. They will schedule any received task to a free Http object
    for route in ROUTES.iter() {
        pool.push(
            task_type_scheduler(
                route_receivers.pop_at(0).expect("Not enough Receivers"),
                *route,
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
async fn task_type_scheduler(
    shared_receiver: Receiver<TaskHandle>,
    route: fn(ChannelId, GuildId) -> Route,
    guild_id: GuildId,
    channel_id: ChannelId,
    worker: Vec<Arc<Http>>,
) -> SchedulerError {
    let mut pool: Vec<Pin<Box<dyn Future<Output = SchedulerError> + Send>>> = Vec::new();

    for w in worker.iter() {
        pool.push(Box::pin(task_http_loop(
            shared_receiver.clone(),
            route,
            guild_id,
            channel_id,
            w.clone(),
        )));
    }

    futures::future::select_all(pool).await.0
}

//Makes sure Http is ready for Task, then tries to get one, for executing it
async fn task_http_loop(
    shared_receiver: Receiver<TaskHandle>,
    route: fn(ChannelId, GuildId) -> Route,
    guild_id: GuildId,
    channel_id: ChannelId,
    worker: Arc<Http>,
) -> SchedulerError {
    loop {
        let ready = {
            let routes_map = worker.ratelimiter.routes();
            let routes_map = routes_map.read().await;
            let target_route = route(channel_id, guild_id);
            let rate_limit = match routes_map.get(&target_route) {
                None => {
                    let msg = SchedulerError::RateLimitError(target_route, guild_id, channel_id);
                    error!("{:?}", &msg);
                    return msg;
                }
                Some(rl) => rl,
            };
            let rate_limit = rate_limit.lock().await;
            if rate_limit.remaining() > 0 {
                Ok(())
            } else {
                Err(match rate_limit.reset_after() {
                    None => {
                        let msg =
                            SchedulerError::ResetTimeError(target_route, guild_id, channel_id);
                        error!("{:?}", &msg);
                        return msg;
                    }
                    Some(reset) => reset,
                })
            }
        };

        match ready {
            Ok(_) => {
                shared_receiver
                    .recv()
                    .await
                    .expect("Task splitter was dropped")
                    .run(worker.clone())
                    .await;
            }
            Err(duration) => {
                smol::Timer::after(duration).await;
            }
        }
    }
}
