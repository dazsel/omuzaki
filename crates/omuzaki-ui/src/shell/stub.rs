//! A runner with no document behind it: what the shell is wired to until the app builds the real executor.
//!
//! Every query is empty and every edit is refused as unavailable, so the panels show their empty states and the
//! shell's undo and redo keys have a real `Arc<dyn Runner>` to call. The app swaps in the real one at its wiring point.

use std::sync::mpsc::Receiver;

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};

/// The service name an unavailable error carries.
const SERVICE: &str = "document";

/// A [`Runner`] that holds nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullRunner;

impl Runner for NullRunner {
    fn ops(&self) -> Vec<OpInfo> {
        Vec::new()
    }

    fn run(&self, _req: Request) -> Result<Outcome, OpError> {
        Err(OpError::unavailable(SERVICE))
    }

    fn begin(&self, _label: &str, _actor: &Actor) -> TxId {
        TxId(0)
    }

    fn commit(&self, _tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        Err(OpError::not_found("no open transaction"))
    }

    fn rollback(&self, _tx: TxId, _actor: &Actor) -> Result<(), OpError> {
        Err(OpError::not_found("no open transaction"))
    }

    fn undo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }

    fn redo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }

    fn subscribe(&self, _grants: &Grants) -> Result<Receiver<Event>, OpError> {
        // The sender is dropped at once, so the stream is empty and ends.
        Ok(std::sync::mpsc::channel().1)
    }

    fn get(&self, _path: &DocPath, _grants: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }

    fn log(&self, _after_seq: u64, _limit: usize, _grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_holds_nothing_and_refuses_edits() {
        let (runner, grants) = (NullRunner, Grants::person());
        assert_eq!(runner.ops().len(), 0);
        assert_eq!(runner.log(0, 10, &grants).expect("an empty log").len(), 0);
        assert_eq!(runner.undo(&Actor::System, &grants).expect("nothing to undo"), None);
        assert!(runner.subscribe(&grants).expect("a stream").try_recv().is_err());
        let request = Request {
            op: "x.y".into(),
            args: Value::Null,
            mode: omuzaki_ops_api::Mode::Apply,
            actor: Actor::System,
            grants,
            cause: None,
            tx: None,
            base: None,
        };
        assert!(runner.run(request).is_err());
    }
}
