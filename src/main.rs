use serenity::http::CacheHttp;
use serenity::Client;

mod scheduler;

#[tokio::main]
async fn main() {
    println!("Hello, world!");

    let mut client = Client::builder("token")
        .await
        .expect("Error creating client");
    let http = client.cache_and_http.http();
    let http2 = client.cache_and_http.http();
}
