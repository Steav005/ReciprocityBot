use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub bots: HashMap<String, String>,
    pub guilds: HashMap<String, GuildConfig>,
    pub lavalink: LavalinkConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GuildConfig {
    pub guild_id: u64,
    pub channel_id: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LavalinkConfig {
    pub address: String,
    pub password: String,
}

impl Config {
    pub fn new(file: String) -> Result<Config, String> {
        let error_message =
            |error: &dyn std::fmt::Display| format!("{}, file: {}", error.to_string(), file);

        Ok(serde_yaml::from_reader(BufReader::new(
            File::open(&file).map_err(|e| error_message(&e))?,
        ))
        .map_err(|e| error_message(&e))?)
    }
}
