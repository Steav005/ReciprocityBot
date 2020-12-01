#![allow(dead_code)]

use crate::event_handler::ReciprocityEventHandler;
use crate::scheduler::TaskHandle;
use lavalink_rs::model::UserId;
use serenity::http::Http;
use serenity::model::channel::Embed;
use serenity::model::id::{ChannelId, MessageId};
use std::hash::{Hash, Hasher};
use smol::channel::{Receiver, Sender};

enum MessageContent {
    Embed(Vec<Embed>),
    Plain(String),
}

struct Message<'a> {
    id: MessageId,
    sender: (UserId, &'a Http),
    channel: ChannelId,
    content: MessageContent,
    hash: u64,
}

impl<'a> Hash for Message<'a> {
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

impl<'a> Message<'a> {
    async fn update(&mut self) -> Result<(), ()> {
        unimplemented!();
    }
}

impl<'a> PartialEq for Message<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

pub struct MessageManager<'a> {
    task_handler: Sender<TaskHandle>,
    event_handler: ReciprocityEventHandler,
    player: Vec<Message<'a>>,
    bots: Vec<(UserId, &'a Http)>,
}

impl<'a> MessageManager<'a> {
    pub fn new(
        bots: Vec<(UserId, &'a Http)>,
        task_handler: Sender<TaskHandle>,
        event_handler: ReciprocityEventHandler,
    ) -> Self {
        MessageManager {
            task_handler,
            event_handler,
            player: Vec::new(),
            bots,
        }
    }
}
