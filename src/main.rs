#![allow(dead_code)]

use clap::{App, Arg};
use reciprocity_bot::config::Config;
use reciprocity_bot::{ReciprocityBot, ReciprocityError};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("RUST_LOG", "reciprocity_bot=info,reciprocity_bot=debug");
    tracing_subscriber::fmt::init();

    let matches = App::new("ReciprocityBot")
        .version(env!("CARGO_PKG_VERSION"))
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .value_name("FILE")
                .help("Sets a custom config file")
                .takes_value(true),
        )
        .get_matches();
    let config_file = matches
        .value_of("config")
        .unwrap_or("config.yml")
        .to_string();
    let config = Arc::new(Config::new(config_file)?);

    //Build threaded RunTime
    let threaded_rt = tokio::runtime::Runtime::new()?;

    //Async main block
    threaded_rt.block_on::<Pin<Box<dyn Future<Output = Result<(), ReciprocityError>>>>>(
        Box::pin(ReciprocityBot::run(config)),
    )?;

    Ok(())
}
