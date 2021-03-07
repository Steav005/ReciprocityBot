#![allow(dead_code)]

use crate::scheduler::TaskHandle;
use crate::voice_handler::VoiceTask;
use async_compat::CompatExt;
use futures::{Future, FutureExt, Stream};
use serenity::http::Http;
use serenity::model::channel::Embed;
use serenity::model::id::{ChannelId, MessageId, UserId};
use smol::channel::{Receiver, Sender};
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

enum MessageContent {
    Embed(Vec<Embed>),
    Plain(String),
}

struct Message {
    id: MessageId,
    sender: (UserId, Arc<Http>),
    channel: ChannelId,
    content: MessageContent,
    timeout: Option<Duration>,
    hash: u64,
}

impl Hash for Message {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let embeds = match &self.content {
            MessageContent::Embed(e) => e,
            MessageContent::Plain(msg) => {
                msg.hash(state);
                return;
            }
        };

        for e in embeds.iter() {
            //Skip Author
            e.colour.0.hash(state);
            if let Some(desc) = &e.description {
                desc.hash(state);
            }
            for f in e.fields.iter() {
                f.value.hash(state);
                f.name.hash(state);
                f.inline.hash(state);
            }
            if let Some(foot) = &e.footer {
                if let Some(icon) = &foot.icon_url {
                    icon.hash(state);
                }
                if let Some(proxy) = &foot.proxy_icon_url {
                    proxy.hash(state);
                }
                foot.text.hash(state);
            }
            if let Some(img) = &e.image {
                img.height.hash(state);
                img.width.hash(state);
                img.proxy_url.hash(state);
                img.url.hash(state);
            }
            //Skip Kind
            //Skip Provider
            if let Some(thumb) = &e.thumbnail {
                thumb.height.hash(state);
                thumb.width.hash(state);
                thumb.proxy_url.hash(state);
                thumb.url.hash(state);
            }
            //Skip Timestamp
            if let Some(title) = &e.title {
                title.hash(state);
            }
            if let Some(url) = &e.url {
                url.hash(state);
            }
            //Skip Video
        }
    }
}

impl Message {
    async fn update(&mut self) -> Result<(), ()> {
        unimplemented!();
    }
}

impl PartialEq for Message {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

pub struct MessageManager {
    //event_stream: Receiver<Event>,
    task_handler: Sender<TaskHandle>,
    //voice_handler: Sender<VoiceTask>,
    player: Vec<Message>,
    bots: Vec<(UserId, Arc<Http>)>,
}

impl MessageManager {
    pub fn new(
        bots: Vec<(UserId, Arc<Http>)>,
        //event_stream: Receiver<Event>,
        task_handler: Sender<TaskHandle>,
        //voice_handler: Sender<VoiceTask>,
    ) -> (Self, Pin<Box<dyn Future<Output = MessageManagerError>>>) {
        (
            MessageManager {
                //event_stream,
                task_handler,
                //voice_handler,
                player: Vec::new(),
                bots,
            },
            run_async().boxed_local(),
        )
    }
}

#[derive(Debug, Error)]
pub enum MessageManagerError {}

async fn run_async() -> MessageManagerError {
    todo!()
}
