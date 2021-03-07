#![allow(dead_code)]

use crate::scheduler::TaskHandle;
use crate::voice_handler::VoiceTask;
use async_compat::CompatExt;
use futures::{Future, FutureExt, Stream};
use serde_json::Value;
use serenity::http::Http;
use serenity::model::channel::{Embed, Message};
use serenity::model::id::{ChannelId, MessageId, UserId};
use serenity::prelude::SerenityError;
use serenity::utils::MessageBuilder;
use smol::channel::{Receiver, Sender};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
enum MessageError {
    #[error("Message Content is Identical: {0:?}")]
    MessageContentIsIdentical(String),
    #[error("Serenity Error Occurred: {0:?}")]
    SerenityError(SerenityError),
}

struct ReciprocityMessage {
    id: MessageId,
    sender: (UserId, Arc<Http>),
    channel: ChannelId,
    content: String,
}

impl ReciprocityMessage {
    async fn update(&mut self, mut content: String) -> Result<(), MessageError> {
        if self.content.eq(&content) {
            return Err(MessageError::MessageContentIsIdentical(content));
        }
        std::mem::swap(&mut self.content, &mut content);
        let old_content = content;

        let result = self
            .sender
            .1
            .edit_message(
                self.channel.0,
                self.id.0,
                &Value::String(self.content.clone()),
            )
            .compat()
            .await;
        if let Err(e) = result {
            self.content = old_content;
            return Err(MessageError::SerenityError(e));
        }
        Ok(())
    }
}

struct SearchMessage {
    message: ReciprocityMessage,
}

struct MainBotMessage {
    message: ReciprocityMessage,
}

pub struct MessageManager {
    //event_stream: Receiver<Event>,
    task_handler: Sender<TaskHandle>,
    voice_handler: Sender<VoiceTask>,

    bots: Vec<(UserId, Arc<Http>)>,
}

impl MessageManager {
    pub fn new(
        bots: Vec<(UserId, Arc<Http>)>,
        //event_stream: Receiver<Event>,
        task_handler: Sender<TaskHandle>,
        voice_handler: Sender<VoiceTask>,
    ) -> (
        Self,
        Pin<Box<dyn Future<Output = MessageManagerError> + Send>>,
    ) {
        (
            MessageManager {
                //event_stream,
                task_handler,
                voice_handler,
                bots,
            },
            run_async().boxed(),
        )
    }
}

#[derive(Debug, Error)]
pub enum MessageManagerError {}

async fn run_async() -> MessageManagerError {
    todo!()
}
