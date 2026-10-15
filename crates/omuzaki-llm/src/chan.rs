//! The bounded channel between a turn's task and its `TurnStream` (llm-api D1): the task awaits, the consumer blocks
//! on a condition variable, and a full queue stops the task from reading the socket.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::stream::{EventSource, LlmEvent, Next};
use tokio::sync::Notify;

/// Events a turn may have in flight before the task waits for the consumer.
pub(crate) const CAPACITY: usize = 64;

/// What travels through the channel.
enum Message {
    Event(LlmEvent),
    Fail(LlmError),
}

#[derive(Default)]
struct State {
    queue: VecDeque<Message>,
    /// The consumer dropped its end, so the task should stop.
    receiver_gone: bool,
    /// The task will send nothing more.
    finished: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    space: Notify,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock only means a thread panicked; the queue is still a plain list.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The task's end.
pub(crate) struct Sender {
    shared: Arc<Shared>,
    done: bool,
}

/// The consumer's end; an [`EventSource`].
pub(crate) struct Receiver {
    shared: Arc<Shared>,
}

/// A channel of [`CAPACITY`] messages.
pub(crate) fn channel() -> (Sender, Receiver) {
    let shared = Arc::new(Shared { state: Mutex::default(), ready: Condvar::new(), space: Notify::new() });
    (Sender { shared: Arc::clone(&shared), done: false }, Receiver { shared })
}

/// The consumer is gone.
pub(crate) struct Closed;

impl Sender {
    async fn push(&self, message: Message) -> Result<(), Closed> {
        let mut message = Some(message);
        loop {
            let notified = self.shared.space.notified();
            tokio::pin!(notified);
            // Register before looking, so a pop between the look and the await is not missed.
            notified.as_mut().enable();
            {
                let mut state = self.shared.lock();
                if state.receiver_gone {
                    return Err(Closed);
                }
                if state.queue.len() < CAPACITY {
                    state.queue.extend(message.take());
                    self.shared.ready.notify_one();
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    /// Queues `event`, waiting while the queue is full.
    pub(crate) async fn event(&self, event: LlmEvent) -> Result<(), Closed> {
        self.push(Message::Event(event)).await
    }

    /// Queues the turn's failure and ends the channel.
    pub(crate) async fn fail(mut self, error: LlmError) {
        // If the consumer is gone nobody wants the error.
        let _ = self.push(Message::Fail(error)).await;
        self.finish();
    }

    /// Ends the channel after the last event.
    pub(crate) fn finish(&mut self) {
        self.done = true;
        self.shared.lock().finished = true;
        self.shared.ready.notify_all();
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // The task ended without saying how (a panic, or the runtime shut down): the consumer must not read that as a
        // complete turn.
        let mut state = self.shared.lock();
        state.queue.push_back(Message::Fail(LlmError::Network("the turn task ended unexpectedly".into())));
        state.finished = true;
        self.shared.ready.notify_all();
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.shared.lock().receiver_gone = true;
        self.shared.space.notify_one();
    }
}

impl EventSource for Receiver {
    fn poll(&mut self, wait: Duration) -> Result<Next, LlmError> {
        let deadline = Instant::now() + wait;
        let mut state = self.shared.lock();
        loop {
            if let Some(message) = state.queue.pop_front() {
                drop(state);
                self.shared.space.notify_one();
                return match message {
                    Message::Event(event) => Ok(Next::Event(event)),
                    Message::Fail(error) => Err(error),
                };
            }
            if state.finished {
                return Ok(Next::End);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Next::Wait);
            }
            state = self.shared.ready.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> LlmEvent {
        LlmEvent::TextDelta(t.into())
    }

    #[test]
    fn events_arrive_in_order_then_end() {
        let (mut tx, mut rx) = channel();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            assert!(tx.event(text("a")).await.is_ok());
            assert!(tx.event(text("b")).await.is_ok());
        });
        tx.finish();
        assert_eq!(rx.poll(Duration::ZERO), Ok(Next::Event(text("a"))));
        assert_eq!(rx.poll(Duration::ZERO), Ok(Next::Event(text("b"))));
        assert_eq!(rx.poll(Duration::ZERO), Ok(Next::End));
    }

    #[test]
    fn an_empty_open_channel_waits() {
        let (_tx, mut rx) = channel();
        assert_eq!(rx.poll(Duration::ZERO), Ok(Next::Wait));
        let start = Instant::now();
        assert_eq!(rx.poll(Duration::from_millis(30)), Ok(Next::Wait));
        assert!(start.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn a_sender_dropped_without_finishing_is_a_network_error() {
        let (tx, mut rx) = channel();
        drop(tx);
        assert!(matches!(rx.poll(Duration::ZERO), Err(LlmError::Network(_))));
        assert_eq!(rx.poll(Duration::ZERO), Ok(Next::End));
    }

    #[test]
    fn a_full_queue_holds_the_sender_until_the_consumer_reads() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap_or_else(|e| panic!("{e}"));
        let (mut tx, mut rx) = channel();
        let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&sent);
        rt.spawn(async move {
            for i in 0..CAPACITY + 10 {
                if tx.event(text(&i.to_string())).await.is_err() {
                    return;
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            tx.finish();
        });
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), CAPACITY, "back-pressure stops at the bound");
        let mut seen = 0;
        while let Ok(Next::Event(_)) = rx.poll(Duration::from_secs(2)) {
            seen += 1;
        }
        assert_eq!(seen, CAPACITY + 10);
    }

    #[test]
    fn dropping_the_receiver_releases_a_waiting_sender() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap_or_else(|e| panic!("{e}"));
        let (tx, rx) = channel();
        let handle = rt.spawn(async move {
            let mut result = Ok(());
            for i in 0..CAPACITY + 5 {
                result = tx.event(text(&i.to_string())).await;
                if result.is_err() {
                    break;
                }
            }
            result.is_err()
        });
        std::thread::sleep(Duration::from_millis(50));
        drop(rx);
        let released = rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), handle).await });
        assert!(matches!(released, Ok(Ok(true))));
    }
}
