use crate::scheduler::{TaskScheduler};
use serenity::http::CacheHttp;
use serenity::model::id::{ChannelId, GuildId};
use serenity::Client;

mod scheduler;
mod guild_handler;

#[tokio::main]
async fn main() {
    println!("Hello, world!");

    let client = Client::builder("token")
        .await
        .expect("Error creating client");
    let http = client.cache_and_http.http();

    let (_scheduler, run) = TaskScheduler::new(GuildId(0), ChannelId(0), vec![http]);
    run.await;
}
