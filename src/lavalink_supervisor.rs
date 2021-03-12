use crate::config::Config;
use crate::lavalink_handler::LavalinkHandler;
use lavalink_rs::LavalinkClient;
use serenity::model::id::UserId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct LavalinkSupervisor {
    lavalink_bots: Arc<HashMap<UserId, Arc<RwLock<Option<LavalinkClient>>>>>,
    event_handler: LavalinkHandler,
    config: Arc<Config>,
}

impl LavalinkSupervisor {
    pub async fn new(
        bots: Vec<UserId>,
        event_handler: LavalinkHandler,
        config: Arc<Config>,
    ) -> LavalinkSupervisor {
        let mut lavalink_bots = HashMap::new();
        for bot in bots {
            let lavalink = Self::generate_new(bot, event_handler.clone(), config.clone()).await;
            lavalink_bots.insert(bot, Arc::new(RwLock::new(lavalink)));
        }

        LavalinkSupervisor {
            lavalink_bots: Arc::new(lavalink_bots),
            event_handler,
            config,
        }
    }

    pub fn get_bots(&self) -> Vec<UserId> {
        self.lavalink_bots.keys().copied().collect()
    }

    pub async fn request_current(&self, bot: UserId) -> Option<LavalinkClient> {
        self.lavalink_bots.get(&bot)?.read().await.clone()
    }

    pub async fn request_new(&self, bot: UserId, old: LavalinkClient) -> Option<LavalinkClient> {
        let rw = self.lavalink_bots.get(&bot)?;
        let current = rw.read().await.clone();
        if current
            .as_ref()
            .map_or(true, |c| Arc::ptr_eq(&c.inner, &old.inner))
        {
            let mut lock = rw.write().await;
            let current = lock.clone();
            //Double Check
            if current
                .as_ref()
                .map_or(false, |c| !Arc::ptr_eq(&c.inner, &old.inner))
            {
                return current;
            }
            *lock = Self::generate_new(bot, self.event_handler.clone(), self.config.clone()).await;
            return lock.clone();
        }
        current
    }

    async fn generate_new(
        bot_id: UserId,
        event_handler: LavalinkHandler,
        config: Arc<Config>,
    ) -> Option<LavalinkClient> {
        LavalinkClient::builder(bot_id)
            .set_host(&config.lavalink.address)
            .set_password(&config.lavalink.password)
            .set_is_ssl(true)
            .build(event_handler.clone())
            .await
            .ok()
    }
}
