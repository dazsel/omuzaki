//! A plugin's worker thread (design §5.1): it owns the plugin's store, runs every entry call in order, and refuses new
//! ones `limited` once [`WORKER_QUEUE`] are waiting. It ends after a fault, after `shutdown`, or when dropped.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, channel, sync_channel};
use std::thread::JoinHandle;

use omuzaki_plugin_api::PluginError;
use omuzaki_plugin_api::limits::WORKER_QUEUE;
use wasmtime::component::Component;

use crate::engine::PluginEngine;
use crate::instance::{Fault, InstanceSetup, LoadedPlugin, StartError};

/// How an entry call on the worker ended.
pub(crate) type Reply = Result<(), Fault>;

/// An entry call for the worker. `approval-resolved` joins with the op bridge, which queues the requests it answers.
enum Job {
    Shutdown(std::sync::mpsc::Sender<Reply>),
}

/// Why starting a worker failed.
pub(crate) enum Failed {
    /// Refused before any guest code ran.
    Refused(PluginError),
    /// The start code or `init` faulted.
    Faulted(Fault),
}

/// The handle on a running worker. Dropping it closes the queue and joins the thread.
pub(crate) struct Worker {
    jobs: Option<SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts the thread `plugin:<name>`: it instantiates `component`, runs `init` with `config`, reports how that went,
    /// then serves entry calls until it faults, shuts down or is dropped.
    pub(crate) fn start(
        name: &str,
        engine: Arc<PluginEngine>,
        component: Component,
        setup: InstanceSetup,
        config: String,
    ) -> Result<Self, Failed> {
        let (jobs, queue) = sync_channel::<Job>(WORKER_QUEUE);
        let (started_tx, started) = channel::<Result<(), Failed>>();
        let thread = std::thread::Builder::new()
            .name(format!("plugin:{name}"))
            .spawn(move || serve(&engine, &component, setup, &config, &started_tx, &queue))
            .map_err(|e| Failed::Refused(PluginError::Instantiate(format!("the worker thread did not start: {e}"))))?;
        let worker = Self { jobs: Some(jobs), thread: Some(thread) };
        match started.recv() {
            Ok(Ok(())) => Ok(worker),
            Ok(Err(failed)) => Err(failed),
            Err(_) => Err(Failed::Refused(PluginError::Instantiate("the worker thread stopped while starting".into()))),
        }
    }

    /// Runs `shutdown` (effects denied) and waits for it; the thread then ends.
    pub(crate) fn shutdown(&self) -> Result<Reply, PluginError> {
        self.ask(Job::Shutdown)
    }

    fn ask(&self, job: impl FnOnce(std::sync::mpsc::Sender<Reply>) -> Job) -> Result<Reply, PluginError> {
        let gone = || PluginError::Instantiate("the plugin's worker has stopped".into());
        let jobs = self.jobs.as_ref().ok_or_else(gone)?;
        let (reply, answer) = channel();
        match jobs.try_send(job(reply)) {
            Ok(()) => answer.recv().map_err(|_| gone()),
            Err(TrySendError::Full(_)) => {
                Err(PluginError::limit(format!("{WORKER_QUEUE} calls already wait for this plugin")))
            }
            Err(TrySendError::Disconnected(_)) => Err(gone()),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            // The thread ends once its queue is closed; a panic inside it has already been reported as a fault.
            let _joined = thread.join();
        }
    }
}

fn serve(
    engine: &PluginEngine,
    component: &Component,
    setup: InstanceSetup,
    config: &str,
    started: &std::sync::mpsc::Sender<Result<(), Failed>>,
    queue: &Receiver<Job>,
) {
    let mut plugin = match LoadedPlugin::start(engine, component, setup) {
        Ok(plugin) => plugin,
        Err(StartError::Refused(error)) => {
            let _told = started.send(Err(Failed::Refused(error)));
            return;
        }
        Err(StartError::Faulted(fault)) => {
            let _told = started.send(Err(Failed::Faulted(fault)));
            return;
        }
    };
    if let Err(fault) = plugin.init(config) {
        let _told = started.send(Err(Failed::Faulted(fault)));
        return;
    }
    if started.send(Ok(())).is_err() {
        return;
    }
    // Until the op bridge brings `approval-resolved`, the one entry call after `init` is `shutdown`; a closed queue
    // (the worker dropped) just drops the instance.
    if let Ok(Job::Shutdown(reply)) = queue.recv() {
        let _told = reply.send(plugin.shutdown());
    }
}
