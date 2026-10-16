//! Layout writes on a worker thread, so a slow disk (a network home folder, a busy antivirus) never stalls a frame.
//!
//! Jobs run in the order they were sent, each write is atomic ([`LayoutStore::save`]), and dropping the [`Saver`]
//! finishes what is queued before the thread ends, so closing the window keeps the last arrangement.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use super::layout::Layout;
use super::persist::LayoutStore;

enum Job {
    Save(String, Layout),
    Reset(String),
    /// Answered once every job before it has finished.
    Flush(Sender<()>),
}

/// Writes layouts in the background.
pub struct Saver {
    jobs: Option<Sender<Job>>,
    worker: Option<JoinHandle<()>>,
}

impl Saver {
    /// Starts the worker thread for `store`.
    ///
    /// # Errors
    /// The operating system refused to start a thread.
    pub fn spawn(store: LayoutStore) -> std::io::Result<Self> {
        let (jobs, queue) = channel();
        let worker = std::thread::Builder::new().name("layout-saver".to_owned()).spawn(move || run(&store, &queue))?;
        Ok(Self { jobs: Some(jobs), worker: Some(worker) })
    }

    /// Queues saving `layout` under `key`.
    pub fn save(&self, key: &str, layout: &Layout) {
        self.send(Job::Save(key.to_owned(), layout.clone()));
    }

    /// Queues forgetting the layout saved under `key`.
    pub fn reset(&self, key: &str) {
        self.send(Job::Reset(key.to_owned()));
    }

    /// Waits until everything queued so far is on disk.
    pub fn flush(&self) {
        let (done, wait) = channel();
        self.send(Job::Flush(done));
        if wait.recv().is_err() {
            tracing::warn!("the layout writer stopped before it finished");
        }
    }

    fn send(&self, job: Job) {
        if let Some(jobs) = &self.jobs
            && jobs.send(job).is_err()
        {
            tracing::warn!("the layout writer has stopped; this change is not saved");
        }
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        // Closing the queue lets the worker finish what is in it and end.
        self.jobs = None;
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("the layout writer panicked");
        }
    }
}

fn run(store: &LayoutStore, queue: &Receiver<Job>) {
    for job in queue {
        match job {
            Job::Save(key, layout) => {
                if let Err(error) = store.save(&key, &layout) {
                    tracing::warn!(layout = key, %error, "could not save the layout");
                }
            }
            Job::Reset(key) => {
                if let Err(error) = store.reset(&key) {
                    tracing::warn!(layout = key, %error, "could not forget the saved layout");
                }
            }
            Job::Flush(done) => {
                if done.send(()).is_err() {
                    tracing::debug!("nobody is waiting for the flush any more");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::layout::Node;
    use super::*;

    #[test]
    fn jobs_run_in_order_and_dropping_finishes_the_queue() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = LayoutStore::new(dir.path().join("layouts"));
        let layout = Layout::new(Node::pane("viewport"));
        let saver = Saver::spawn(store.clone()).expect("a thread");
        saver.save("a", &layout);
        saver.save("b", &layout);
        saver.reset("a");
        saver.flush();
        assert!(matches!(store.load("a"), Ok(None)), "reset came after save");
        assert_eq!(store.load("b").expect("readable"), Some(layout.clone()));
        saver.save("c", &layout);
        drop(saver);
        assert_eq!(store.load("c").expect("readable"), Some(layout), "dropping writes what was queued");
    }
}
