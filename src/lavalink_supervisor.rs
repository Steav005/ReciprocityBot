use crate::config::LavalinkConfig;
use crate::lavalink_handler::LavalinkHandler;
use lavalink_rs::LavalinkClient;
use log::warn;
use serenity::model::id::UserId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const CONNECTION_ATTEMPT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct LavalinkSupervisor {
    lavalink_bots: Arc<HashMap<UserId, RwLock<LavalinkClient>>>,
    event_handler: LavalinkHandler,
    config: LavalinkConfig,
}

impl LavalinkSupervisor {
    pub async fn new(
        bots: Vec<UserId>,
        event_handler: LavalinkHandler,
        config: LavalinkConfig,
    ) -> LavalinkSupervisor {
        let mut map = HashMap::new();
        for bot in bots {
            let lavalink = Self::generate_new(bot, event_handler.clone(), &config).await;
            map.insert(bot, RwLock::new(lavalink));
        }

        LavalinkSupervisor {
            lavalink_bots: Arc::new(map),
            event_handler,
            config,
        }
    }

    pub fn get_bots(&self) -> Vec<UserId> {
        self.lavalink_bots.keys().copied().collect()
    }

    pub async fn request_current(&self, bot: UserId) -> Option<LavalinkClient> {
        Some(self.lavalink_bots.get(&bot)?.read().await.clone())
    }

    pub async fn request_new(&self, bot: UserId, old: LavalinkClient) -> Option<LavalinkClient> {
        let rw = self.lavalink_bots.get(&bot)?;
        let current = rw.read().await.clone();
        if Arc::ptr_eq(&current.inner, &old.inner) {
            let mut lock = rw.write().await;
            let current = lock.clone();
            //Double Check
            if !Arc::ptr_eq(&current.inner, &old.inner) {
                return Some(current);
            }
            *lock = Self::generate_new(bot, self.event_handler.clone(), &self.config).await;
        }
        Some(current)
    }

    async fn generate_new(
        bot_id: UserId,
        event_handler: LavalinkHandler,
        config: &LavalinkConfig,
    ) -> LavalinkClient {
        loop {
            let client = LavalinkClient::builder(bot_id)
                .set_host(&config.address)
                .set_password(&config.password)
                .set_is_ssl(true)
                .build(event_handler.clone())
                .await;
            match client {
                Ok(client) => {
                    return client;
                }
                Err(err) => {
                    warn!("Error, building Lavalink Client: {:?}", err);
                    tokio::time::sleep(CONNECTION_ATTEMPT_INTERVAL).await;
                }
            }
        }
    }
}
