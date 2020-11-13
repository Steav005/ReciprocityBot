#![allow(dead_code)]

use arrayvec::ArrayVec;
use futures::prelude::*;
use log::{debug, error, warn};
use serenity::http::routing::Route;
use serenity::http::Http;
use serenity::model::id::{ChannelId, GuildId};
use serenity::static_assertions::_core::pin::Pin;
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::mpsc::error::{SendError, TrySendError};
use tokio::sync::mpsc::{Receiver as MpscReceiver, Sender as MpscSender};
use tokio::sync::oneshot::{Receiver as OneShotReceiver, Sender as OneShotSender};
use tokio::sync::watch::{Receiver as WatchReceiver, Sender as WatchSender};
use tokio::sync::{mpsc, oneshot, watch, RwLock};

const QUEUE_LIMIT: usize = 10000;

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

#[derive(Debug)]
pub enum TaskError {
    FailedExecution(),
    Dropped(),
}

pub enum Task {
    DeleteMessageReaction(u64), //Route 0,
    DeleteMessage(u64),         //Route 1,
                                //TODO Add more Tasks
}

pub const ROUTES_NUM: usize = 2;
pub static ROUTES: [fn(ChannelId, GuildId) -> Route; ROUTES_NUM] =
    [channel_id_message, channel_id_message_id_reaction];

const fn channel_id_message_id_reaction(channel: ChannelId, _: GuildId) -> Route {
    Route::ChannelsIdMessagesIdReactions(channel.0)
}

const fn channel_id_message(channel: ChannelId, _: GuildId) -> Route {
    Route::ChannelsIdMessages(channel.0)
}

pub struct TaskHandler {
    pub receiver: Final<OneShotReceiver<Result<(), TaskError>>>,
    sender: OneShotSender<Result<(), TaskError>>,
    pub task: Final<Task>,
}

impl TaskHandler {
    pub fn new(task: Task) -> TaskHandler {
        let (sender, receiver) = oneshot::channel();

        TaskHandler {
            receiver: Final::new(receiver),
            sender,
            task: Final::new(task),
        }
    }

    pub async fn run(self, _client: &Http) {
        let result: Result<(), TaskError> = match self.task.deref() {
            Task::DeleteMessageReaction(_) => {
                //TODO implement
                Ok(())
            }
            Task::DeleteMessage(_) => {
                //TODO implement
                Ok(())
            }
        };

        if let Err(err) = self.sender.send(result) {
            debug!("Task Error Result was dropped: {:?}", err);
        }
    }

    pub fn drop(self) {
        if let Err(err) = self.sender.send(Err(TaskError::Dropped())) {
            debug!("Task Error was dropped: {:?}", err.unwrap_err())
        }
    }

    pub fn get_route_id(&self) -> usize {
        match self.task.t {
            Task::DeleteMessageReaction(_) => 0,
            Task::DeleteMessage(_) => 1,
        }
    }
}

pub struct TaskScheduler {
    pub task_sender: Final<MpscSender<TaskHandler>>,
    pub guild_id: Final<GuildId>,
    pub channel_id_sender: Final<WatchSender<ChannelId>>,
}

impl TaskScheduler {
    pub fn new<'a>(
        guild_id: GuildId,
        channel_id: ChannelId,
        worker: Vec<&'a Http>,
    ) -> (TaskScheduler, Pin<Box<dyn Future<Output = ()> + 'a>>) {
        let (task_sender, task_receiver) = mpsc::channel(QUEUE_LIMIT);
        let (channel_id_sender, channel_id_receiver) = watch::channel(channel_id);

        let scheduler = TaskScheduler {
            task_sender: Final::new(task_sender),
            guild_id: Final::new(guild_id),
            channel_id_sender: Final::new(channel_id_sender),
        };

        (
            scheduler,
            Box::pin(run_async(
                worker,
                guild_id,
                channel_id_receiver,
                task_receiver,
            )),
        )
    }

    pub fn try_send_task(&self, task: TaskHandler) -> Result<(), TrySendError<TaskHandler>> {
        self.task_sender.try_send(task)
    }

    pub async fn send_task(&self, task: TaskHandler) -> Result<(), SendError<TaskHandler>> {
        self.task_sender.send(task).await
    }
}

//Receive Task and send it to the proper scheduler
async fn split_receive(
    mut receiver: MpscReceiver<TaskHandler>,
    sender: [MpscSender<TaskHandler>; ROUTES_NUM],
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
    task_receiver: MpscReceiver<TaskHandler>,
) {
    let mut pool: Vec<Pin<Box<dyn Future<Output = ()>>>> = Vec::new();
    let mut senders = ArrayVec::new();
    let mut receivers: ArrayVec<[MpscReceiver<TaskHandler>; ROUTES_NUM]> = ArrayVec::new();

    for _ in 0..ROUTES.len() {
        let (s, r) = mpsc::channel(QUEUE_LIMIT);
        senders.push(s);
        receivers.push(r);
    }

    //This one is for taking any received tasks and pushing it to the appropriate receiver
    let sender = senders.into_inner().unwrap();
    pool.push(Box::pin(split_receive(task_receiver, sender, guild_id)));

    //Here we start the receivers. They will schedule any received task to a free Http object
    for route in ROUTES.iter() {
        pool.push(Box::pin(task_type_scheduler(
            receivers.pop_at(0).expect("Not enough Receivers"),
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
    receiver: MpscReceiver<TaskHandler>,
    route: fn(ChannelId, GuildId) -> Route,
    guild_id: GuildId,
    channel_id: WatchReceiver<ChannelId>,
    worker: Vec<&Http>,
) {
    let mut pool: Vec<Pin<Box<dyn Future<Output = ()>>>> = Vec::new();
    let receiver = Arc::new(RwLock::new(receiver));

    for w in worker.iter() {
        pool.push(Box::pin(task_http_loop(
            receiver.clone(),
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
    receiver: Arc<RwLock<MpscReceiver<TaskHandler>>>,
    route: fn(ChannelId, GuildId) -> Route,
    guild_id: GuildId,
    channel_id: WatchReceiver<ChannelId>,
    worker: &Http,
) {
    loop {
        let rate = {
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

        match rate {
            Ok(_) => {
                let task = receiver.write().await.recv().await;
                task.expect("Task splitter was dropped").run(worker).await;
            }
            Err(duration) => {
                tokio::time::sleep(duration).await;
            }
        }
    }
}
