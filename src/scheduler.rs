#![allow(dead_code)]

use crate::scheduler::TaskError::FailedExecution;
use arrayvec::ArrayVec;
use async_std::sync::Arc;
use futures::prelude::*;
use log::{debug, error, warn};
use serenity::http::routing::Route;
use serenity::http::Http;
use serenity::model::channel::ReactionType;
use serenity::model::id::{ChannelId, GuildId, MessageId, UserId};
use serenity::prelude::SerenityError;
use std::ops::Deref;
use std::pin::Pin;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Receiver as MpscReceiver, Sender as MpscSender};
use tokio::sync::oneshot::{Receiver as OneShotReceiver, Sender as OneShotSender};
use tokio::sync::watch::{Receiver as WatchReceiver, Sender as WatchSender};
use tokio::sync::{mpsc, oneshot, watch, RwLock};

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
#[derive(Debug)]
pub enum TaskError {
    FailedExecution(SerenityError),
    BotDoesNotExist(),
    Dropped(),
}

///The Tasks, that can be enqueued
pub enum Task {
    DeleteMessageReaction(ChannelId, MessageId, UserId, ReactionType),
    DeleteMessage(ChannelId, MessageId),
    AddMessageReaction(ChannelId, MessageId, ReactionType),
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
    pub receiver: Final<OneShotReceiver<Result<(), TaskError>>>,
    sender: OneShotSender<Result<(), TaskError>>,
    pub task: Final<Task>,
}

impl TaskHandle {
    ///Builds a TaskHandle from a Task
    pub fn new(task: Task) -> TaskHandle {
        let (sender, receiver) = oneshot::channel();

        TaskHandle {
            receiver: Final::new(receiver),
            sender,
            task: Final::new(task),
        }
    }

    ///Tries to complete the Task with the help of an Http Instance
    pub async fn run(self, client: &Http) {
        let result: Result<(), TaskError> = match self.task.deref() {
            Task::DeleteMessageReaction(channel, message, user, reaction) => client
                .delete_reaction(channel.0, message.0, Some(user.0), reaction)
                .await
                .map_err(FailedExecution),
            Task::DeleteMessage(channel, message) => client
                .delete_message(channel.0, message.0)
                .await
                .map_err(FailedExecution),
            Task::AddMessageReaction(channel, message, reaction) => client
                .create_reaction(channel.0, message.0, reaction)
                .await
                .map_err(FailedExecution),
        };

        if let Err(err) = self.sender.send(result) {
            debug!("Task Error Result was dropped: {:?}", err);
        }
    }

    ///Drops the Task/TaskHandler
    pub fn drop(self) {
        if let Err(err) = self.sender.send(Err(TaskError::Dropped())) {
            debug!("Task Error was dropped: {:?}", err.unwrap_err())
        }
    }

    pub fn drop_no_bot(self) {
        if let Err(err) = self.sender.send(Err(TaskError::BotDoesNotExist())) {
            debug!("Task Error was dropped: {:?}", err.unwrap_err())
        }
    }

    ///Returns the Route, the underlying Task requires
    pub fn get_route_id(&self) -> usize {
        let route = match self.task.t {
            Task::DeleteMessageReaction(_, _, _, _) => channel_id_message_id_reaction,
            Task::DeleteMessage(_, _) => channel_id_message,
            Task::AddMessageReaction(_, _, _) => channel_id_message_id_reaction_self,
        } as usize;

        ROUTES
            .iter()
            .position(|f| *f as usize == route)
            .expect("Route not Found")
    }
}

///Schedules Tasks within a Guild
pub struct TaskScheduler {
    task_sender: Final<MpscSender<TaskHandle>>,
    guild_id: Final<GuildId>,
    channel_id_sender: Final<WatchSender<ChannelId>>,
}

impl TaskScheduler {
    ///Builds a new Scheduler for a Guild based on the used Https
    pub fn new<'a>(
        guild_id: GuildId,
        channel_id: ChannelId,
        worker: Vec<&'a Http>,
    ) -> (Self, Pin<Box<dyn Future<Output = ()> + 'a>>) {
        let (task_sender, task_receiver) = mpsc::channel(QUEUE_LIMIT);
        let (channel_id_sender, channel_id_receiver) = watch::channel(channel_id);

        let scheduler = TaskScheduler {
            task_sender: Final::new(task_sender),
            guild_id: Final::new(guild_id),
            channel_id_sender: Final::new(channel_id_sender),
        };

        (
            // The Scheduler used for enqueuing new Tasks
            scheduler,
            // Future which will run the Scheduler
            Box::pin(run_async(
                worker,
                guild_id,
                channel_id_receiver,
                task_receiver,
            )),
        )
    }

    /// Get a Task Sender Clone for enqueuing tasks
    pub fn get_task_sender(&self) -> MpscSender<TaskHandle>{
        self.task_sender.t.clone()
    }

    /// Changes the channel_id in case the Bot Channel Changed
    pub fn change_channel_id(&self, channel_id: ChannelId) {
        if self.channel_id_sender.t.send(channel_id).is_err() {
            let msg = format!(
                "Channel Change Receiver dropped. Guild: {:?}",
                self.guild_id.t
            );
            error!("{}", &msg);
            panic!(msg)
        }
    }

    ///Does not Block if the Buffer is full
    pub fn try_send_task(&self, task: TaskHandle) -> Result<(), ()> {
        if let Err(err) = self.task_sender.try_send(task) {
            match err {
                TrySendError::Full(task) => {
                    warn!("Buffer Full. Guild: {:?}", self.guild_id.t);
                    task.drop();
                    return Err(());
                }
                TrySendError::Closed(_) => {
                    let msg = format!("Task Receiver dropped. Guild: {:?}", self.guild_id.t);
                    error!("{}", &msg);
                    panic!(msg)
                }
            }
        }
        Ok(())
    }

    ///Blocks if the Buffer is full
    pub async fn send_task(&self, task: TaskHandle) {
        if self.task_sender.send(task).await.is_err() {
            let msg = format!("Task Receiver dropped. Guild: {:?}", self.guild_id.t);
            error!("{}", &msg);
            panic!(msg)
        };
    }
}

/// Receive Task and send it to the proper scheduler
async fn split_receive(
    mut receiver: MpscReceiver<TaskHandle>,
    sender: [MpscSender<TaskHandle>; ROUTES.len()],
    guild_id: GuildId,
) {
    loop {
        let task = match receiver.recv().await {
            None => {
                let msg = format!("Task Sender dropped. Guild: {:?}", guild_id);
                error!("{}", &msg);
                panic!(msg);
            }
            Some(task) => task,
        };
        let route_id = task.get_route_id();

        if let Err(err) = sender[route_id].try_send(task) {
            match err {
                TrySendError::Full(task) => {
                    warn!(
                        "Buffer Full. Guild: {:?}, RouteID: {:?}",
                        guild_id, route_id
                    );
                    task.drop();
                }
                TrySendError::Closed(_) => {
                    let msg = format!(
                        "Task Receiver dropped. Guild: {:?}, RouteID: {:?}",
                        guild_id, route_id
                    );
                    error!("{}", &msg);
                    panic!(msg);
                }
            }
        }
    }
}

async fn run_async(
    worker: Vec<&Http>,
    guild_id: GuildId,
    channel_id_receiver: WatchReceiver<ChannelId>,
    task_receiver: MpscReceiver<TaskHandle>,
) {
    let mut pool: Vec<Pin<Box<dyn Future<Output = ()>>>> = Vec::new();
    let mut route_senders = ArrayVec::new();
    let mut route_receivers: ArrayVec<[MpscReceiver<TaskHandle>; ROUTES.len()]> = ArrayVec::new();

    for _ in 0..ROUTES.len() {
        let (s, r) = mpsc::channel(QUEUE_LIMIT);
        route_senders.push(s);
        route_receivers.push(r);
    }

    //This one is for taking any received tasks and pushing it to the appropriate receiver
    let route_senders = route_senders.into_inner().unwrap();
    pool.push(Box::pin(split_receive(
        task_receiver,
        route_senders,
        guild_id,
    )));

    //Here we start the receivers. They will schedule any received task to a free Http object
    for route in ROUTES.iter() {
        pool.push(Box::pin(task_type_scheduler(
            route_receivers.pop_at(0).expect("Not enough Receivers"),
            *route,
            guild_id,
            channel_id_receiver.clone(),
            worker.clone(),
        )))
    }

    //Here we make them all run
    futures::future::select_all(pool).await;
    //If any one of those exited, we exit the program
    panic!("Scheduler exited")
}

//Hosts all Https for this Task
async fn task_type_scheduler(
    shared_receiver: MpscReceiver<TaskHandle>,
    route: fn(ChannelId, GuildId) -> Route,
    guild_id: GuildId,
    channel_id: WatchReceiver<ChannelId>,
    worker: Vec<&Http>,
) {
    let mut pool: Vec<Pin<Box<dyn Future<Output = ()>>>> = Vec::new();
    let shared_receiver = Arc::new(RwLock::new(shared_receiver));

    for w in worker.iter() {
        pool.push(Box::pin(task_http_loop(
            shared_receiver.clone(),
            route,
            guild_id,
            channel_id.clone(),
            w,
        )));
    }

    futures::future::select_all(pool).await;
    panic!("Http Loop Exited")
}

//Makes sure Http is ready for Task, then tries to get one, for executing it
async fn task_http_loop(
    shared_receiver: Arc<RwLock<MpscReceiver<TaskHandle>>>,
    route: fn(ChannelId, GuildId) -> Route,
    guild_id: GuildId,
    channel_id: WatchReceiver<ChannelId>,
    worker: &Http,
) {
    loop {
        let ready = {
            let routes_map = worker.ratelimiter.routes();
            let routes_map = routes_map.read().await;
            let channel_id = *channel_id.borrow();
            let target_route = route(channel_id, guild_id);
            let rate_limit = match routes_map.get(&target_route) {
                None => {
                    let msg = format!(
                        "Couldn't get Rate Limit for Route: {:?}. Guild: {:?}, Channel: {:?}",
                        target_route, guild_id, channel_id
                    );
                    error!("{}", &msg);
                    panic!(msg);
                }
                Some(rl) => rl,
            };
            let rate_limit = rate_limit.lock().await;
            if rate_limit.remaining() > 0 {
                Ok(())
            } else {
                Err(match rate_limit.reset_after() {
                    None => {
                        let msg = format!(
                            "Couldn't get Reset Time for Route: {:?}. Guild: {:?}, Channel: {:?}",
                            target_route, guild_id, channel_id
                        );
                        error!("{}", &msg);
                        panic!(msg);
                    }
                    Some(reset) => reset,
                })
            }
        };

        match ready {
            Ok(_) => {
                shared_receiver
                    .write()
                    .await
                    .recv()
                    .await
                    .expect("Task splitter was dropped")
                    .run(worker)
                    .await;
            }
            Err(duration) => {
                tokio::time::sleep(duration).await;
            }
        }
    }
}
