use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::iter::Iterator;
use std::pin::Pin;
use std::sync::Arc;

use serenity::async_trait;
use serenity::futures::stream::StreamExt;
use serenity::model::prelude::{ChannelId, GuildId};
use serenity::CacheAndHttp;
use strum::IntoEnumIterator;
use thiserror::Error;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

use crate::guild::ReciprocityGuild;
use crate::task_handle::{Task, TaskHandle, TaskHandlerError, TaskRoute};

#[async_trait]
pub trait GuildScheduler: Send + Sync {
    ///Inits Scheduler
    /// - Creates Route Scheduler for every existing Route
    fn init_scheduler(&mut self);

    ///Process Task
    /// - Returns Error, if channel is full
    /// - Returns Task if run successfully
    async fn process(
        &self,
        task: impl Task + 'static,
    ) -> Result<Pin<Box<dyn Task>>, SchedulerError>;
}

#[async_trait]
impl GuildScheduler for ReciprocityGuild {
    fn init_scheduler(&mut self) {
        let routes: HashMap<_, _> = TaskRoute::iter()
            .map(|route| {
                (
                    route,
                    RouteScheduler::new(
                        route,
                        self.id,
                        self.channel,
                        self.bots
                            .values()
                            .cloned()
                            .map(|(cache_http, _)| cache_http)
                            .collect(),
                    ),
                )
            })
            .collect();
        *self.route_scheduler.borrow_mut() = Arc::new(routes);
    }

    async fn process(
        &self,
        task: impl Task + 'static,
    ) -> Result<Pin<Box<dyn Task>>, SchedulerError> {
        let route = task.route();
        let (handle, task_result) = TaskHandle::new(task);

        if let Some(scheduler) = self.route_scheduler.get(&route) {
            //Enqueue handle
            scheduler.enqueue(handle)?;
            //Await Task Result
            return match task_result.await {
                Ok(res) => res.map_err(SchedulerError::TaskHandlerError),
                Err(rec_err) => Err(SchedulerError::TaskResultReceiveError(rec_err)),
            };
        }
        Err(SchedulerError::NoRouteScheduler(route))
    }
}

pub(in crate::guild) struct RouteScheduler {
    send: Sender<TaskHandle>,
}

impl RouteScheduler {
    pub fn new(
        route: TaskRoute,
        guild: GuildId,
        channel: ChannelId,
        mut bots: Vec<Arc<CacheAndHttp>>,
    ) -> RouteScheduler {
        let (send, receive): (Sender<TaskHandle>, Receiver<TaskHandle>) =
            tokio::sync::mpsc::channel(100);
        let receive = Arc::new(Mutex::new(ReceiverStream::new(receive).peekable()));

        for bot in bots.drain(..) {
            let receive = receive.clone();

            let _: JoinHandle<Result<(), ()>> = tokio::spawn(async move {
                let routes_map = bot.http.ratelimiter.routes();
                let target_route = route.get_serenity_route(channel, guild);
                loop {
                    if let Some(ratelimit) = routes_map.read().await.get(&target_route) {
                        let lock = ratelimit.lock().await;
                        let remaining = lock.remaining();
                        let reset = lock.reset();
                        drop(lock);

                        if remaining <= 0 {
                            if let Some(time) = reset {
                                if let Err(until) = time.elapsed() {
                                    tokio::time::sleep(until.duration()).await
                                }
                            }
                        }
                    }

                    //Lock Receiver
                    let mut receive_lock = receive.lock().await;
                    //Wait until we receive something
                    Pin::new((*receive_lock).borrow_mut())
                        .peek()
                        .await
                        .ok_or(())?;
                    //Check if we actually know this guild
                    if bot.cache.guild_field(guild, |_| ()).await.is_none() {
                        //If not drop, drop lock and restart loop
                        drop(receive_lock);
                        continue;
                    }
                    //Get real Task (Should evaluate instantaneously)
                    let task = (*receive_lock).next().await.ok_or(())?;
                    //Drop Lock so others can get more tasks
                    drop(receive_lock);
                    //Execute Task and ignore outcome
                    task.run(bot.http.clone()).await.ok();
                }
            });
        }

        RouteScheduler { send }
    }

    pub fn enqueue(&self, task: TaskHandle) -> Result<(), SchedulerError> {
        self.send.try_send(task).map_err(SchedulerError::SendError)
    }
}

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("Error Sending Task, {0:?}")]
    SendError(TrySendError<TaskHandle>),
    #[error("Could not receive Task Result because sender was dropped: {0:?}")]
    TaskResultReceiveError(RecvError),
    #[error("Could not find scheduler for Route: {0:?}")]
    NoRouteScheduler(TaskRoute),
    #[error("Error occurred processing the Handle: {0:?}")]
    TaskHandlerError(TaskHandlerError),
}
