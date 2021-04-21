#![allow(dead_code)]

use clap::{App, Arg};
use reciprocity_bot::config::Config;
use reciprocity_bot::{ReciprocityBot, ReciprocityError};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
//use log::LevelFilter;
//use log4rs::append::file::FileAppender;
//use log4rs::encode::pattern::PatternEncoder;
//use log4rs::config::{Appender, Root};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("RUST_LOG", "reciprocity_bot=info,reciprocity_bot=debug");
    tracing_subscriber::fmt::init();

    //let logfile = FileAppender::builder()
    //    .encoder(Box::new(PatternEncoder::new("{l} - {m}\n")))
    //    .build("log/output.log").unwrap();
    //let config = log4rs::Config::builder()
    //    .appender(Appender::builder().build("logfile", Box::new(logfile)))
    //    .build(Root::builder()
    //        .appender("logfile")
    //        .build(LevelFilter::Info))?;
    //log4rs::init_config(config).unwrap();

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
