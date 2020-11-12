use futures::prelude::*;
use futures::stream::FuturesUnordered;
use futures::stream::Stream;
use serenity::http::routing::Route;
use serenity::http::Http;
use serenity::model::guild::ActionMember::RoleUpdate;
use serenity::static_assertions::_core::cell::Cell;
use serenity::static_assertions::_core::pin::Pin;
use serenity::static_assertions::_core::task::{Context, Poll};
use serenity::static_assertions::_core::time::Duration;
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::TryInto;
use std::io::Read;
use std::ops::Deref;
use std::sync::Arc;
use tokio::pin;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Notify, RwLock};
use arrayvec::ArrayVec;

const QUEUE_LIMIT: usize = 10000;

pub struct Final<T> {
    t: T,
}

impl<T> Final<T> {
    fn new(t: T) -> Final<T> {
        Final { t }
    }
}

impl<T> Deref for Final<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.t
    }
}

const TASK_VARIANT_COUNT: usize = 3;

pub enum Task {
    Task1(),
    Task2(),
    Task3(),
}

impl Task {
    pub async fn run(self, client: &Http) {
        //TODO delete dummy run
        client.get_current_application_info().await.unwrap();
    }

    pub fn route_from_index(index: usize) -> Route {
        //TODO implement Routes for Tasks
        Route::None
    }

    pub fn route(&self) -> Route {
        Self::route_from_index(self.to_int())
    }

    pub fn to_int(&self) -> usize {
        match self {
            Task::Task1() => 0,
            Task::Task2() => 1,
            Task::Task3() => 2,
        }
    }
}

struct TaskScheduler<'a> {
    worker: Vec<&'a Http>,
    mpsc_receiver: Receiver<Task>,
    mpsc_sender: Sender<Task>,
}

impl<'a> TaskScheduler<'a> {
    pub fn new(worker: Vec<&'a Http>) -> TaskScheduler<'a> {
        let (sender, receiver) = mpsc::channel(QUEUE_LIMIT);

        TaskScheduler {
            worker,
            mpsc_receiver: receiver,
            mpsc_sender: sender,
        }
    }

    pub fn get_sender(&self) -> Sender<Task> {
        self.mpsc_sender.clone()
    }

    pub async fn run_async(mut self) -> ! {
        let mut pool: Vec<Pin<Box<dyn Future<Output = ()>>>> = Vec::new();
        let mut sender = ArrayVec::new();
        let mut receiver: ArrayVec<[Receiver<Task>; TASK_VARIANT_COUNT]> = ArrayVec::new();

        for _ in 0..TASK_VARIANT_COUNT{
            let (s, r) = mpsc::channel(QUEUE_LIMIT);
            sender.push(s);
            receiver.push(r);
        }

        //This one is for taking any received tasks and pushing it to the appropriate receiver
        let sender = sender.into_inner().unwrap();
        pool.push(Box::pin(split_receive(self.mpsc_receiver, sender)));

        //Here we start the receivers. They will schedule any received task to a free Http object
        for i in 0..TASK_VARIANT_COUNT{
            pool.push(Box::pin(task_type_scheduler(receiver.pop_at(0).unwrap(), Task::route_from_index(i), self.worker.clone())))
        }

        //Here we make them all run
        futures::future::select_all(pool).await;
        //If any one of those exited, we exit the program
        panic!("Scheduler exited")
    }
}

//Receive Task and send it to the proper scheduler
async fn split_receive(mut receiver: Receiver<Task>, sender: [Sender<Task>; TASK_VARIANT_COUNT]){
    loop {
        let task = receiver.recv().await.expect("Sender was dropped");
        if sender[task.to_int()].try_send(task).is_err(){
            //Just drop it
        }
    }
}

//Hosts all Https for this Task
async fn task_type_scheduler(mut receiver: Receiver<Task>, route: Route, worker: Vec<&Http>){
    let mut pool: Vec<Pin<Box<dyn Future<Output = ()>>>> = Vec::new();
    let receiver = Arc::new(RwLock::new(receiver));

    for w in worker.iter(){
        pool.push(Box::pin(task_http_loop(receiver.clone(), route, w)));
    }

    futures::future::select_all(pool).await;
    panic!("Http Loop Exited")
}

//Makes sure Http is ready for Task, then tries to get one, for executing it
async fn task_http_loop(receiver: Arc<RwLock<Receiver<Task>>>, route: Route, worker: &Http){
    loop{
        let rate = {
            let routes =  worker.ratelimiter.routes();
            let map = routes.read().await;
            let rate_limiter = map.get(&route).unwrap().lock().await;
            if rate_limiter.remaining() > 0{
                Ok(())
            } else {
                Err(rate_limiter.reset_after().expect("Reset after failed"))
            }
        };

        match rate{
            Ok(_) => {
                let task = receiver.write().await.recv().await;
                task.expect("Task splitter was dropped").run(worker).await;
            }
            Err(duration) => {
                tokio::time::sleep(duration).await;
            }
        }
    }
}