#![allow(dead_code, clippy::too_many_arguments)]

use crate::config::{Config, LavalinkConfig};
use crate::player::{Player, PlayerError, PlayerStatus, Playback};
use arc_swap::ArcSwap;
use async_compat::CompatExt;
use event_listener::Event as SmolEvent;
use futures::prelude::future::Either;
use futures::prelude::*;
use lavalink_rs::gateway::LavalinkEventHandler as EventHandler;
use lavalink_rs::model::{
    PlayerUpdate as LavalinkUpdate, Track, TrackFinish as LavalinkTrackFinish,
    TrackStart as LavalinkTrackStart, UserId,
};
use lavalink_rs::LavalinkClient;
use log::{debug, error, info, warn};
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::id::ChannelId;
use serenity::model::prelude::GuildId;
use smol::channel::{Receiver, RecvError, Sender};
use smol::{channel, Timer};
use songbird::Songbird;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const TASK_QUEUE_LIMIT: usize = 1000;
const CONNECTION_ATTEMPT_INTERVAL: Duration = Duration::from_secs(5);

///Tasks which are meant for lavalink instances
pub enum VoiceTask {
    ///Target Guild and Target Channel
    Connect(GuildId, ChannelId),

    ///Target Guild and Target BotID
    Disconnect(GuildId, UserId),
    Next(GuildId, UserId),
    Previous(GuildId, UserId),
    PausePlay(GuildId, UserId),
    Clear(GuildId, UserId),
    RepeatOne(GuildId, UserId),
    RepeatAll(GuildId, UserId),

    /// Target Guild, BotID, Query and Callback Function
    Search(
        GuildId,
        UserId,
        String,
        Sender<Result<Vec<Track>, Either<VoiceHandlerError, PlayerError>>>,
    ),
}

impl VoiceTask {
    ///Returns either a Bot UserID or a desired ChannelID
    pub fn identifier(&self) -> Either<UserId, ChannelId> {
        match self {
            VoiceTask::Connect(_, channel) => Either::Right(*channel),
            VoiceTask::Disconnect(_, bot) => Either::Left(*bot),
            VoiceTask::Next(_, bot) => Either::Left(*bot),
            VoiceTask::Previous(_, bot) => Either::Left(*bot),
            VoiceTask::PausePlay(_, bot) => Either::Left(*bot),
            VoiceTask::Clear(_, bot) => Either::Left(*bot),
            VoiceTask::RepeatOne(_, bot) => Either::Left(*bot),
            VoiceTask::RepeatAll(_, bot) => Either::Left(*bot),
            VoiceTask::Search(_, bot, _, _) => Either::Left(*bot),
        }
    }

    ///Returns the set GuildId
    pub fn guild(&self) -> GuildId {
        match self {
            VoiceTask::Connect(guild, _) => *guild,
            VoiceTask::Disconnect(guild, _) => *guild,
            VoiceTask::Next(guild, _) => *guild,
            VoiceTask::Previous(guild, _) => *guild,
            VoiceTask::PausePlay(guild, _) => *guild,
            VoiceTask::Clear(guild, _) => *guild,
            VoiceTask::RepeatOne(guild, _) => *guild,
            VoiceTask::RepeatAll(guild, _) => *guild,
            VoiceTask::Search(guild, _, _, _) => *guild,
        }
    }
}

pub type PlayerStatusMap = Arc<HashMap<GuildId, HashMap<UserId, (Notify, PlayerStatusSwap)>>>;
pub type PlayerStatusSwap = Arc<ArcSwap<Option<PlayerStatus>>>;
pub type Notify = Arc<SmolEvent>;

///Does the VoiceHandling for the entire Bot Network <br>
///Manages Lavalink related stuff
#[derive(Clone)]
pub struct VoiceHandler {
    task_sender: Sender<VoiceTask>,
    player_states: PlayerStatusMap,
}

impl VoiceHandler {
    pub fn new(
        bots: Vec<(UserId, Arc<Http>, Arc<Songbird>)>,
        config: Config,
    ) -> (
        Self,
        Pin<Box<dyn Future<Output = VoiceHandlerError> + Send>>,
    ) {
        let (task_sender, task_receiver) = channel::bounded(TASK_QUEUE_LIMIT);

        let mut player_states = HashMap::new();
        for guild in config.guilds.values() {
            let mut guild_player_map: HashMap<_, (Notify, PlayerStatusSwap)> = HashMap::new();
            for (bot_id, _, _) in bots.iter() {
                guild_player_map.insert(
                    *bot_id,
                    (
                        Arc::new(SmolEvent::new()),
                        Arc::new(ArcSwap::new(Arc::new(None))),
                    ),
                );
            }
            player_states.insert(GuildId(guild.guild_id), guild_player_map);
        }
        let player_states = Arc::new(player_states);

        (
            VoiceHandler {
                task_sender,
                player_states: player_states.clone(),
            },
            run_async(task_receiver, bots, player_states, config).boxed(),
        )
    }

    /// Get a VoiceTask Sender Clone for enqueuing VoiceTasks
    pub fn get_task_sender(&self) -> Sender<VoiceTask> {
        self.task_sender.clone()
    }
}

///Errors which can emerge inside the VoiceHandler
#[derive(Debug, Error, Clone)]
pub enum VoiceHandlerError {
    #[error("{0:?}")]
    ReceiveError(RecvError),
    #[error("Could not send Task to bot: {0}")]
    SendError(UserId),
    #[error("Bot does not exist: {0}")]
    BotDoesNotExist(UserId),
}

///Global Async run for managing voice related stuff
async fn run_async(
    voice_task_receiver: Receiver<VoiceTask>,
    bots: Vec<(UserId, Arc<Http>, Arc<Songbird>)>,
    player_states: PlayerStatusMap,
    config: Config,
) -> VoiceHandlerError {
    //future pool for collection all the futures
    let mut pool: Vec<Pin<Box<dyn Future<Output = VoiceHandlerError> + Send>>> = Vec::new();
    //Postboxes for every guild in every bot
    //This means we have .Bot * Guilds. Postboxes
    let mut postboxes = HashMap::new();
    //A Vector which holds BotIDs and Hashmaps which contain if a bot is free for specific guild
    let mut bot_free_list = Vec::new();

    //For each bot
    for (id, http, songbird) in bots {
        //Hashmap which contains guilds as keys and a bool if the guild is free as the value
        //This is used for the bot_free_list vector
        let mut bot_guild_map_splitter = HashMap::new();
        //Hashmap for the Bot, which contains a TaskReceiver and the free value behind a guildID key
        let mut bot_guild_map_bot = HashMap::new();
        //For each Guild
        for guild in config.guilds.values() {
            let free = Arc::new(ArcSwap::from_pointee(false));
            let guild = GuildId(guild.guild_id);
            bot_guild_map_splitter.insert(guild, free.clone());

            //Channel for Tasks, for a specific bot and guild
            let (send, rec) = channel::bounded(TASK_QUEUE_LIMIT);
            postboxes.insert((id, guild), send);
            bot_guild_map_bot.insert(guild, (free, rec));
        }

        //Add Hashmap for this bot to the list made for the splitter
        bot_free_list.push((id, bot_guild_map_splitter));

        //Add async function to function pool
        pool.push(
            bot_run_async(
                id,
                songbird.clone(),
                config.lavalink.clone(),
                bot_guild_map_bot,
                player_states.clone(),
                http,
            )
            .boxed(),
        );
    }
    //Add Sorter to async function pool
    pool.push(sort_task_async(voice_task_receiver, postboxes, bot_free_list).boxed());

    //Await any future ending
    futures::future::select_all(pool).await.0
}

///Contains information about a bot<br>
///UserID and Hashmap which contains which Guilds it can join, because there is no active player.
type FreeBotInfo = (UserId, HashMap<GuildId, Arc<ArcSwap<bool>>>);

///Sorts incoming voice task and distributes them based on desires bot/channel and guild
async fn sort_task_async(
    receiver: Receiver<VoiceTask>,
    postboxes: HashMap<(UserId, GuildId), Sender<VoiceTask>>,
    free_bots: Vec<FreeBotInfo>,
) -> VoiceHandlerError {
    loop {
        //Get next Task for distribution
        let task = match receiver.recv().await {
            Err(err) => {
                let msg = VoiceHandlerError::ReceiveError(err);
                error!("{:?}", &msg);
                return msg;
            }
            Ok(task) => task,
        };

        //Either get target bot or determine free bot
        let bot_guild = match task {
            VoiceTask::Connect(guild, _) => {
                //Find first bot for guild where free is true
                let bot = free_bots.iter().find(|(_, free)| match free.get(&guild) {
                    None => false,
                    Some(free) => *free.load().as_ref(),
                });
                match bot {
                    None => {
                        debug!("No free bot left for Guild: {:?}", guild);
                        continue;
                    }
                    Some((bot, _)) => (*bot, guild),
                }
            }
            VoiceTask::Disconnect(guild, bot) => (bot, guild),
            VoiceTask::Next(guild, bot) => (bot, guild),
            VoiceTask::Previous(guild, bot) => (bot, guild),
            VoiceTask::PausePlay(guild, bot) => (bot, guild),
            VoiceTask::Clear(guild, bot) => (bot, guild),
            VoiceTask::RepeatOne(guild, bot) => (bot, guild),
            VoiceTask::RepeatAll(guild, bot) => (bot, guild),
            VoiceTask::Search(guild, bot, _, _) => (bot, guild),
        };

        //Send task to bot
        match postboxes.get(&bot_guild) {
            None => {
                let msg = VoiceHandlerError::BotDoesNotExist(bot_guild.0);
                error!("{:?}", &msg);
                return msg;
            }
            Some(postbox) => {
                if postbox.send(task).await.is_err() {
                    let msg = VoiceHandlerError::SendError(bot_guild.0);
                    error!("{:?}", &msg);
                    return msg;
                }
            }
        }
    }
}

///Async run for all guilds this bot belongs to
async fn bot_run_async(
    id: UserId,
    songbird: Arc<Songbird>,
    config: LavalinkConfig,
    guild_map: HashMap<GuildId, (Arc<ArcSwap<bool>>, Receiver<VoiceTask>)>,
    player_states: PlayerStatusMap,
    http: Arc<Http>,
) -> VoiceHandlerError {
    //Send map for the EventHandler, contains Sender for each Guild
    let mut event_handler_sender_map = HashMap::new();
    //Backup map with relevant information for restarting "bot_guild_run_async"
    //Contains Guild as key and free, task_receive and event_receive as Values
    let mut backup_map = HashMap::new();

    //For each Guild fill backup_map and event_handler_sender_map
    for (guild, (can_join, receive)) in guild_map.iter() {
        //Quick check if this Bot knows the guild
        if http.get_guild(guild.0).compat().await.is_ok() {
            can_join.store(Arc::from(true));
        }

        //Make channel for the Lavalink events
        let (send, rec) = channel::bounded(TASK_QUEUE_LIMIT);
        //Insert in both hashmaps
        backup_map.insert(*guild, (can_join.clone(), receive.clone(), rec));
        event_handler_sender_map.insert(*guild, send);
    }
    //Make EventHandler with initialized event_handler_sender_map
    let event_handler = LavalinkEventHandler {
        sender: Arc::new(event_handler_sender_map),
    };

    //Async Function pool
    let mut pool = Vec::new();
    //lavalink client pre-initialized
    let mut lavalink = build_lavalink_client(id, &config, event_handler.clone()).await;
    //Initially fill pool with values from backup_map
    for (guild, (free, task_rec, event_rec)) in backup_map.iter() {
        let (player_change, player_state) = player_states
            .get(guild)
            .expect("Guild missing in player_states")
            .get(&id)
            .expect("Bot missing in player_states")
            .clone();

        pool.push(
            bot_guild_run_async(
                id,
                lavalink.clone(),
                songbird.clone(),
                free.clone(),
                player_change,
                player_state,
                task_rec.clone(),
                event_rec.clone(),
            )
            .boxed(),
        )
    }

    //Main loop
    loop {
        //Wait and see if any future ends
        let (result, _, rem) = futures::future::select_all(pool).await;
        let (guild, _, dead_lavalink) = match result {
            //If the ending reason was a dead lavalink instance continue
            Either::Left((guild, error, dead_lavalink)) => (guild, error, dead_lavalink),
            //Else its not fixable and we return the error
            Either::Right(err) => {
                return err;
            }
        };

        //If this Lavalink is dead, get a new one
        if Arc::ptr_eq(&dead_lavalink.inner, &lavalink.inner) {
            lavalink = build_lavalink_client(id, &config, event_handler.clone()).await
        }
        //Get Backup data
        let (free, task_receiver, event_receiver) = backup_map.get(&guild).unwrap().clone();

        //Move pool and push bot run again with backup data
        pool = rem;
        let (player_change, player_state) = player_states
            .get(&guild)
            .expect("Guild missing in player_states")
            .get(&id)
            .expect("Bot missing in player_states")
            .clone();
        pool.push(
            bot_guild_run_async(
                id,
                lavalink.clone(),
                songbird.clone(),
                free,
                player_change,
                player_state,
                task_receiver,
                event_receiver,
            )
            .boxed(),
        )
    }
}

/// Async run for a bot with a specific Guild
async fn bot_guild_run_async(
    id: UserId,
    lavalink: LavalinkClient,
    songbird: Arc<Songbird>,
    free: Arc<ArcSwap<bool>>,
    player_change: Notify,
    player_state: PlayerStatusSwap,
    task_receiver: Receiver<VoiceTask>,
    event_receiver: Receiver<LavalinkEvent>,
) -> Either<(GuildId, PlayerError, LavalinkClient), VoiceHandlerError> {
    //Initialize futures
    let mut task_rec = task_receiver.recv().boxed();
    let mut event_rec = event_receiver.recv().boxed();

    //Empty player initialization
    let mut player: Option<Player> = None;

    //Main loop of bot_guild_run_async
    loop {
        //Wait for any of the two receiver to receive anything
        match futures::future::select(task_rec, event_rec).await {
            //If a Task was received
            Either::Left((task, event)) => {
                //set task_rec to a new future and move the event future to event_rec
                task_rec = task_receiver.recv().boxed();
                event_rec = event;

                match task {
                    Err(e) => {
                        let msg = VoiceHandlerError::ReceiveError(e);
                        error!("{:?}", msg);
                        return Either::Right(msg);
                    }
                    Ok(task) => {
                        //Match Task and act accordingly
                        match task {
                            VoiceTask::Connect(guild, channel) => {
                                if player.is_none() {
                                    player = match Player::new(
                                        id,
                                        guild,
                                        channel,
                                        lavalink.clone(),
                                        &songbird,
                                        player_change.clone(),
                                        player_state.clone(),
                                    )
                                    .await
                                    {
                                        Ok(player) => {
                                            free.store(Arc::new(false));
                                            Some(player)
                                        }
                                        Err(e) => {
                                            if let PlayerError::LavalinkError(_, _) = e {
                                                return Either::Left((guild, e, lavalink));
                                            }
                                            //TODO is this continue reasonable ?
                                            continue;
                                        }
                                    }
                                }
                            }
                            VoiceTask::Disconnect(guild, _) => {
                                if let Some(player) = player {
                                    if let Err(e) = player.disconnect().await {
                                        if let PlayerError::LavalinkError(_, _) = e {
                                            return Either::Left((guild, e, lavalink));
                                        }
                                    }
                                }
                                player = None;
                                free.store(Arc::new(true));
                            }
                            VoiceTask::Next(_, _) => {
                                if let Some(player) = &player {
                                    player.skip().await;
                                }
                            }
                            VoiceTask::Previous(_, _) => {
                                if let Some(player) = &player {
                                    player.back_skip().await;
                                }
                            }
                            VoiceTask::PausePlay(guild, _) => {
                                if let Some(player) = &player {
                                    if let Err(e) = player.dynamic_pause_resume().await{
                                        if let PlayerError::LavalinkError(_, _) = e{
                                            return Either::Left((guild, e, lavalink))
                                        }
                                    }
                                }
                            }
                            VoiceTask::Clear(_, _) => {
                                if let Some(player) = &player {
                                    player.clear_queue();
                                }
                            }
                            VoiceTask::RepeatOne(_, _) => {
                                if let Some(player) = &player{
                                    player.playback(Playback::SingleLoop)
                                }
                            }
                            VoiceTask::RepeatAll(_, _) => {
                                if let Some(player) = &player{
                                    player.playback(Playback::AllLoop)
                                }
                            }
                            VoiceTask::Search(_, _, query, callback) => {
                                if let Some(player) = &player{
                                    player.search(query, callback);
                                }
                            }
                        }
                    }
                }
            }
            //If an Event was received
            Either::Right((event, task)) => {
                //move task future to task_rec and set event_rec to new future
                task_rec = task;
                event_rec = event_receiver.recv().boxed();

                match event {
                    Err(e) => {
                        let msg = VoiceHandlerError::ReceiveError(e);
                        error!("{:?}", msg);
                        return Either::Right(msg);
                    }
                    //Match event and pass it to the player if the player exists
                    Ok(event) => match event {
                        LavalinkEvent::Update(update) => {
                            if let Some(player) = player.as_mut() {
                                player.update(update);
                            }
                        }
                        LavalinkEvent::TrackStart(start) => {
                            if let Some(player) = player.as_mut() {
                                player.track_start(start);
                            }
                        }
                        LavalinkEvent::TrackEnd(end) => {
                            if let Some(player) = player.as_mut() {
                                player.track_end(end).await;
                            }
                        }
                    },
                }
            }
        }
    }
}

///LavalinkClient Builder which will retry to connect to the defined server indefinitely
async fn build_lavalink_client(
    id: UserId,
    config: &LavalinkConfig,
    event_handler: LavalinkEventHandler,
) -> LavalinkClient {
    loop {
        let client = LavalinkClient::builder(id)
            .set_host(&config.address)
            .set_password(&config.password)
            .set_is_ssl(true)
            .build(event_handler.clone())
            .compat()
            .await;
        match client {
            Ok(client) => {
                return client;
            }
            Err(err) => {
                error!("Error, building Lavalink Client: {:?}", err);
                Timer::after(CONNECTION_ATTEMPT_INTERVAL).await;
            }
        }
    }
}

///Lavalink Event Handler which will pass the Events to their corresponding guild
#[derive(Clone)]
struct LavalinkEventHandler {
    sender: Arc<HashMap<GuildId, Sender<LavalinkEvent>>>,
}

#[async_trait]
impl EventHandler for LavalinkEventHandler {
    async fn player_update(&self, _: LavalinkClient, event: LavalinkUpdate) {
        let guild = GuildId(event.guild_id);
        if let Some(sender) = self.sender.get(&guild) {
            if sender.send(LavalinkEvent::Update(event)).await.is_err() {
                error!("Dropped Receiver, {:?}", guild);
            }
        }
    }

    async fn track_start(&self, _: LavalinkClient, event: LavalinkTrackStart) {
        let guild = GuildId(event.guild_id);
        if let Some(sender) = self.sender.get(&GuildId(event.guild_id)) {
            if sender.send(LavalinkEvent::TrackStart(event)).await.is_err() {
                error!("Dropped Receiver, {:?}", guild);
            }
        }
    }

    async fn track_finish(&self, _: LavalinkClient, event: LavalinkTrackFinish) {
        let guild = GuildId(event.guild_id);
        if let Some(sender) = self.sender.get(&GuildId(event.guild_id)) {
            if sender.send(LavalinkEvent::TrackEnd(event)).await.is_err() {
                error!("Dropped Receiver, {:?}", guild);
            }
        }
    }
}

///Events which we receive from the LavalinkEventHandler
pub enum LavalinkEvent {
    //Stats(LavalinkStats),
    Update(LavalinkUpdate),
    TrackStart(LavalinkTrackStart),
    TrackEnd(LavalinkTrackFinish),
}
