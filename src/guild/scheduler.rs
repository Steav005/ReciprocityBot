use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::iter::Iterator;
use std::pin::Pin;
use std::sync::Arc;

use serenity::futures::stream::StreamExt;
use serenity::model::prelude::{ChannelId, GuildId};

use strum::IntoEnumIterator;
use thiserror::Error;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot::error::RecvError;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

use crate::bots::BotMap;
use crate::task_handle::{Task, TaskHandle, TaskHandlerError, TaskRoute};

#[derive(Clone)]
pub struct GuildScheduler {
    route_scheduler: Arc<HashMap<TaskRoute, RouteScheduler>>,
}

impl GuildScheduler {
    ///Inits Scheduler
    /// - Creates Route Scheduler for every existing Route
    pub fn new(guild: GuildId, channel: ChannelId, bots: Arc<BotMap>) -> Self {
        let routes: HashMap<_, _> = TaskRoute::iter()
            .map(|route| {
                (
                    route,
                    RouteScheduler::new(route, guild, channel, bots.clone()),
                )
            })
            .collect();
        GuildScheduler {
            route_scheduler: Arc::new(routes),
        }
    }

    ///Process Task
    /// - Returns Error, if channel is full
    /// - Returns Task if run successfully
    pub async fn process(
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

    pub async fn process_enqueue(&self, task: impl Task + 'static) -> Result<(), SchedulerError> {
        let route = task.route();
        let (handle, _) = TaskHandle::new(task);

        if let Some(scheduler) = self.route_scheduler.get(&route) {
            //Enqueue handle
            return scheduler.enqueue(handle);
        }
        Err(SchedulerError::NoRouteScheduler(route))
    }
}

pub struct RouteScheduler {
    send: Sender<TaskHandle>,
}

impl RouteScheduler {
    pub fn new(
        route: TaskRoute,
        guild: GuildId,
        channel: ChannelId,
        bots: Arc<BotMap>,
    ) -> RouteScheduler {
        let (send, receive): (Sender<TaskHandle>, Receiver<TaskHandle>) =
            tokio::sync::mpsc::channel(100);
        let receive = Arc::new(Mutex::new(ReceiverStream::new(receive).peekable()));

        for bot in bots.bots() {
            let receive = receive.clone();

            let _: JoinHandle<Result<(), ()>> = tokio::spawn(async move {
                let routes_map = bot.http().ratelimiter.routes();
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
                    if bot.cache().guild_field(guild, |_| ()).await.is_none() {
                        //If not drop, drop lock and restart loop
                        drop(receive_lock);
                        continue;
                    }
                    //Get real Task (Should evaluate instantaneously)
                    let task = (*receive_lock).next().await.ok_or(())?;
                    //Drop Lock so others can get more tasks
                    drop(receive_lock);
                    //Execute Task and ignore outcome
                    task.run(bot.http().clone()).await.ok();
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
