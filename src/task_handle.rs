use crate::context::SearchMessageId;
use log::{debug, error, warn};
use serenity::async_trait;
use serenity::http::routing::Route;
use serenity::http::Http;
use serenity::model::prelude::{ChannelId, GuildId, Message, MessageId, ReactionType, UserId};
use serenity::prelude::SerenityError;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use strum_macros::{EnumCount as EnumCountMacro, EnumIter};
use thiserror::Error;
use tokio::sync::oneshot::{Receiver as OneShotReceiver, Sender as OneShotSender};
use tokio::sync::watch::Sender as WatchSender;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

type PinedTask = Pin<Box<dyn Task>>;

#[derive(Debug)]
pub struct TaskHandle {
    sender: Option<OneShotSender<Result<PinedTask, TaskHandlerError>>>,
    task: Option<PinedTask>,
}

impl TaskHandle {
    pub fn new(
        task: impl Task + 'static,
    ) -> (
        TaskHandle,
        OneShotReceiver<Result<PinedTask, TaskHandlerError>>,
    ) {
        let (sender, receive) = tokio::sync::oneshot::channel();

        (
            TaskHandle {
                sender: Some(sender),
                task: Some(Box::pin(task)),
            },
            receive,
        )
    }

    ///Tries to complete the Task with the help of an Http Instance
    pub async fn run(mut self, client: Arc<Http>) -> Result<(), ()> {
        if let Some(mut task) = self.task.take() {
            let result = task
                .run(client.clone())
                .await
                .map(|_| task)
                .map_err(TaskHandlerError::FailedExecution);

            let mut ret = Ok(());
            if result.is_err() {
                ret = Err(());
            }

            if let Some(sender) = self.sender.take() {
                if let Err(err) = sender.send(result) {
                    debug!("Task Error Result was dropped: {:?}", err);
                }
            }

            return ret;
        }
        Ok(())
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            if let Err(err) = sender.send(Err(TaskHandlerError::Dropped(self.task.take()))) {
                debug!("Task Error was dropped: {:?}", err.unwrap_err())
            }
        }
    }
}

///Any Error that can be returned by any "enqueued" Task
#[derive(Debug, Error)]
pub enum TaskHandlerError {
    #[error("Execution of the Task failed: Serenity Error: {0:?}")]
    FailedExecution(SerenityError),
    #[error("The Task was dropped: {0:?}")]
    Dropped(Option<Pin<Box<dyn Task>>>),
}

///All the Routes which will be used by any Task
#[derive(EnumIter, EnumCountMacro, Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub enum TaskRoute {
    ChannelMessageReaction,
    ChannelMessage,
    ChannelMessageReactionSelf,
    ChannelMessagesBulkDelete,
}

impl TaskRoute {
    pub fn get_serenity_route(&self, channel: ChannelId, _guild: GuildId) -> Route {
        match self {
            TaskRoute::ChannelMessageReaction => Route::ChannelsIdMessagesIdReactions(channel.0),
            TaskRoute::ChannelMessage => Route::ChannelsIdMessages(channel.0),
            TaskRoute::ChannelMessageReactionSelf => {
                Route::ChannelsIdMessagesIdReactionsUserIdType(channel.0)
            }

            TaskRoute::ChannelMessagesBulkDelete => Route::ChannelsIdMessagesBulkDelete(channel.0),
        }
    }
}

#[async_trait]
pub trait Task: Send + Sync + Unpin + Debug + Any {
    async fn run(&mut self, client: Arc<Http>) -> Result<(), SerenityError>;

    fn route(&self) -> TaskRoute;
}

#[derive(Debug)]
pub struct DeleteMessageReactionTask {
    pub channel: ChannelId,
    pub message: MessageId,
    pub user: UserId,
    pub reaction: ReactionType,
}

#[async_trait]
impl Task for DeleteMessageReactionTask {
    async fn run(&mut self, client: Arc<Http>) -> Result<(), SerenityError> {
        client
            .delete_reaction(
                self.channel.0,
                self.message.0,
                Some(self.user.0),
                &self.reaction,
            )
            .await
    }

    fn route(&self) -> TaskRoute {
        TaskRoute::ChannelMessageReaction
    }
}

#[derive(Debug)]
pub struct DeleteMessagePoolTask {
    pub channel: ChannelId,
    pub pool: Arc<Mutex<Vec<MessageId>>>,
}

#[async_trait]
impl Task for DeleteMessagePoolTask {
    async fn run(&mut self, client: Arc<Http>) -> Result<(), SerenityError> {
        let mut pool: Vec<_> = self.pool.lock().await.drain(..).collect();
        if pool.is_empty() {
            return Ok(());
        }

        loop {
            let rem = pool.len();
            if rem == 1 {
                return self
                    .channel
                    .delete_message(client, pool.first().unwrap())
                    .await;
            }
            if rem <= 100 {
                return self.channel.delete_messages(&client, pool.drain(..)).await;
            }
            let res = self
                .channel
                .delete_messages(&client, pool.drain(..100))
                .await;
            if let Err(e) = res {
                warn!("Error Bulk deleting. {:?}, {:?}", self.channel, e);
            }
        }
    }

    fn route(&self) -> TaskRoute {
        TaskRoute::ChannelMessagesBulkDelete
    }
}

#[derive(Debug)]
pub struct AddMessageReactionTask {
    pub channel: ChannelId,
    pub message: MessageId,
    pub reaction: ReactionType,
}

#[async_trait]
impl Task for AddMessageReactionTask {
    async fn run(&mut self, client: Arc<Http>) -> Result<(), SerenityError> {
        client
            .create_reaction(self.channel.0, self.message.0, &self.reaction)
            .await
    }

    fn route(&self) -> TaskRoute {
        TaskRoute::ChannelMessageReactionSelf
    }
}

#[derive(Debug)]
pub struct SendSearchMessage {
    pub channel: ChannelId,
    pub text: String,
    pub uuid: Uuid,
    pub search_messages: Arc<RwLock<HashMap<UserId, SearchMessageId>>>,
    pub callback: WatchSender<Option<Message>>,
}

#[async_trait]
impl Task for SendSearchMessage {
    async fn run(&mut self, client: Arc<Http>) -> Result<(), SerenityError> {
        let relevant = self
            .search_messages
            .read()
            .await
            .values()
            .any(|(_, u)| self.uuid.eq(u));
        if !relevant {
            return Ok(());
        }
        let msg = self
            .channel
            .send_message(client, |m| m.content(self.text.clone()))
            .await;
        if let Ok(msg) = &msg {
            self.callback.send(Some(msg.clone())).ok();
        }
        msg.map(|_| ())
    }

    fn route(&self) -> TaskRoute {
        TaskRoute::ChannelMessage
    }
}
