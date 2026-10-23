//! Sessions on a branch (`docs/design/branches.md` 8.1 to 8.3): the host's branches as a server sees them, the branch
//! built-ins (`branch_create`, `branch_checkpoint`, `branch_switch`; `branch_work` is the connection's, in `hop`), what
//! `session_info` says about the branch, and the review outcomes a later result opens with.

use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use omuzaki_history_api::branch::{
    BranchError, BranchEvent, BranchId, BranchInfo, BranchName, Branches, NewBranch, ReviewState,
};
use omuzaki_ops_api::{Actor, OpError, SessionId};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde_json::{Value, json};

use crate::events::lock;
use crate::render;
use crate::server::McpServer;

/// The MCP server of each lane, so a connection can move to another branch (`branch_work`).
pub trait LaneServers: Send + Sync {
    /// The server over `branch`'s lane, opening the lane if it is closed.
    ///
    /// # Errors
    /// As the host's `Branches::lane`: an unknown branch, or no room for another open lane.
    fn server(&self, branch: &BranchId) -> Result<McpServer, OpError>;
}

/// The host's branches, the lane servers, and the branch this server's lane holds (see
/// [`McpOptions::with_branches`](crate::McpOptions::with_branches)).
#[derive(Clone)]
pub struct BranchHost {
    pub(crate) branches: Arc<dyn Branches>,
    pub(crate) lanes: Arc<dyn LaneServers>,
    pub(crate) branch: BranchId,
    reviews: Arc<ReviewFeed>,
}

impl fmt::Debug for BranchHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BranchHost").field("branch", &self.branch).finish_non_exhaustive()
    }
}

impl BranchHost {
    /// The branches of the host, its lane servers, and `branch`, the one this server's lane holds.
    pub fn new(branches: Arc<dyn Branches>, lanes: Arc<dyn LaneServers>, branch: BranchId) -> Self {
        Self { branches, lanes, branch, reviews: Arc::new(ReviewFeed::default()) }
    }

    /// This lane's branch as the host records it now; `None` for `main` before anything branched (it has no ref yet).
    pub(crate) fn info(&self) -> Option<BranchInfo> {
        self.branches.list().into_iter().find(|b| b.id == self.branch)
    }

    /// This lane's branch's name.
    pub(crate) fn name(&self) -> String {
        self.info().map_or_else(|| BranchName::main().to_string(), |info| info.name.to_string())
    }

    /// The live branch called `name`.
    pub(crate) fn by_name(&self, name: &str) -> Result<BranchInfo, OpError> {
        let wanted = BranchName::new(name).map_err(|e| OpError::from(BranchError::from(e)))?;
        if wanted == BranchName::main() && self.branches.list().iter().all(|b| !b.id.is_main()) {
            return Err(OpError::conflict("nothing has branched yet: every session is on main"));
        }
        self.branches
            .list()
            .into_iter()
            .find(|b| b.name == wanted && b.deleted.is_none())
            .ok_or_else(|| OpError::not_found(format!("no branch {wanted}")).hint("branch_list lists the branches"))
    }

    /// The review outcomes for this lane's branch since `seen`, and the new mark.
    pub(crate) fn reviews_since(&self, seen: usize) -> (Vec<String>, usize) {
        self.reviews.start(&self.branches);
        let notes = lock(&self.reviews.notes);
        let mine = notes.iter().skip(seen).filter(|n| n.branch == self.branch).map(|n| n.text.clone()).collect();
        (mine, notes.len())
    }

    /// How many review outcomes the feed holds now: where a new session starts reading.
    pub(crate) fn reviews_now(&self) -> usize {
        self.reviews.start(&self.branches);
        lock(&self.reviews.notes).len()
    }
}

fn string<'a>(args: &'a JsonObject, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn required<'a>(args: &'a JsonObject, key: &str) -> Result<&'a str, OpError> {
    string(args, key).filter(|s| !s.is_empty()).ok_or_else(|| OpError::invalid(key, format!("`{key}` is required")))
}

/// `branch_create`: a branch from this lane's branch (or `from`), owned by `session` when an agent makes it.
pub(crate) fn create(
    host: &BranchHost,
    args: &JsonObject,
    actor: &Actor,
    session: Option<SessionId>,
) -> Result<CallToolResult, OpError> {
    let name = BranchName::new(required(args, "name")?).map_err(|e| OpError::from(BranchError::from(e)))?;
    let from = match string(args, "from") {
        Some(from) => host.by_name(from)?.id,
        None => host.branch.clone(),
    };
    let mut new = NewBranch::new(name, from.clone());
    if let Some(title) = string(args, "title") {
        new = new.with_title(title);
    }
    if let Some(focus) = args.get("focus").and_then(Value::as_array) {
        new = new.with_focus(focus.iter().filter_map(Value::as_str).map(str::to_owned).collect());
    }
    if let (Actor::Agent { .. }, Some(session)) = (actor, session) {
        new = new.for_session(session);
    }
    let made = host.branches.create(new, actor).map_err(OpError::from)?;
    let from_name = host
        .branches
        .list()
        .into_iter()
        .find(|b| b.id == from)
        .map_or_else(|| "main".to_owned(), |b| b.name.to_string());
    let text = format!(
        "Made branch {} from {from_name} as it is committed now. Move onto it with branch_work {{\"branch\": \"{}\"}}.",
        made.name, made.name
    );
    Ok(render::confirmation(text, json!({ "branch": made })))
}

/// `branch_checkpoint`: a point of this lane's branch, asking for review when `ready`.
pub(crate) fn checkpoint(host: &BranchHost, args: &JsonObject, actor: &Actor) -> Result<CallToolResult, OpError> {
    let label = required(args, "label")?;
    let ready = args.get("ready").and_then(Value::as_bool).unwrap_or(false);
    let note = string(args, "note").map(str::to_owned);
    let point = host
        .branches
        .checkpoint(&host.branch, label, ready.then(|| note.unwrap_or_default()), actor)
        .map_err(OpError::from)?;
    let name = host.name();
    let text = if ready {
        format!(
            "Point {point} of {name} ('{label}') is ready: the person is asked to review it. Their answer opens a later result."
        )
    } else {
        format!("Point {point} of {name} ('{label}') recorded.")
    };
    Ok(render::confirmation(text, json!({ "branch": name, "point": point.to_string(), "ready": ready })))
}

/// `branch_switch`: the person's; an agent is told to move its session instead.
pub(crate) fn switch(host: &BranchHost, args: &JsonObject, actor: &Actor) -> Result<CallToolResult, OpError> {
    let target = host.by_name(required(args, "branch")?)?;
    if matches!(actor, Actor::Agent { .. }) {
        return Err(OpError::denied("switching the project folder is the person's").hint(format!(
            "move your session instead: branch_work {{\"branch\": \"{}\"}}; to hand your work over, branch_checkpoint with ready",
            target.name
        )));
    }
    let switched = host.branches.switch(&target.id, actor).map_err(OpError::from)?;
    let text = format!("The project folder now holds {}.", target.name);
    Ok(render::confirmation(text, json!({ "from": switched.from, "to": switched.to })))
}

/// `result` with `Branch: <name>` added, for `session_info`.
pub(crate) fn with_branch(mut result: CallToolResult, name: &str) -> CallToolResult {
    let line = format!("Branch: {name} (branch_work moves this session to another)");
    match result.content.first().and_then(ContentBlock::as_text) {
        Some(first) => result.content[0] = ContentBlock::text(format!("{}\n{line}", first.text)),
        None => result.content.push(ContentBlock::text(line)),
    }
    if let Some(Value::Object(data)) = result.structured_content.as_mut() {
        data.insert("branch".to_owned(), json!(name));
    }
    result
}

/// `result` opening with the review outcomes in `notes`.
pub(crate) fn with_reviews(mut result: CallToolResult, notes: &[String]) -> CallToolResult {
    if notes.is_empty() {
        return result;
    }
    let lines = notes.iter().map(|n| format!("Review: {n}")).collect::<Vec<_>>().join("\n");
    match result.content.first().and_then(ContentBlock::as_text) {
        Some(first) => result.content[0] = ContentBlock::text(format!("{lines}\n\n{}", first.text)),
        None => result.content.insert(0, ContentBlock::text(lines)),
    }
    if let Some(Value::Object(data)) = result.structured_content.as_mut() {
        data.insert("reviews".to_owned(), json!(notes));
    }
    result
}

/// The review outcomes of every branch, in the order they came, from the host's branch events.
#[derive(Default)]
struct ReviewFeed {
    notes: Mutex<Vec<Note>>,
    started: OnceLock<()>,
}

struct Note {
    branch: BranchId,
    text: String,
}

impl ReviewFeed {
    /// Starts following the host's events, once. Lazily: a lane's server is built before the host's branches can
    /// answer, and the first session is after.
    fn start(self: &Arc<Self>, branches: &Arc<dyn Branches>) {
        self.started.get_or_init(|| {
            let events = branches.subscribe();
            let (feed, branches): (Weak<Self>, Weak<dyn Branches>) = (Arc::downgrade(self), Arc::downgrade(branches));
            let spawned = std::thread::Builder::new().name("omuzaki-mcp-reviews".to_owned()).spawn(move || {
                while let Ok(event) = events.recv() {
                    let BranchEvent::Reviewed { branch, state, .. } = event else { continue };
                    let (Some(feed), Some(branches)) = (feed.upgrade(), branches.upgrade()) else { return };
                    if let Some(text) = describe(branches.as_ref(), &branch, state) {
                        lock(&feed.notes).push(Note { branch, text });
                    }
                }
            });
            if let Err(error) = spawned {
                tracing::warn!(%error, "review outcomes won't reach agents' results");
            }
        });
    }
}

/// "the person merged aria/oval's ready version into main", "the person sent back aria/oval: 'rounder'".
fn describe(branches: &dyn Branches, branch: &BranchId, state: ReviewState) -> Option<String> {
    let all = branches.list();
    let info = all.iter().find(|b| b.id == *branch)?;
    let folder = branches.folder();
    let folder = all.iter().find(|b| b.id == folder).map_or_else(|| "main".to_owned(), |b| b.name.to_string());
    let point = info.review.as_ref().map(|r| format!(" (point {})", r.point)).unwrap_or_default();
    match state {
        ReviewState::Merged => Some(format!("the person merged {}'s ready version{point} into {folder}", info.name)),
        ReviewState::SentBack => {
            let comment =
                info.review.as_ref().and_then(|r| r.note.clone()).map(|c| format!(": '{c}'")).unwrap_or_default();
            Some(format!("the person sent back {}{point}{comment}", info.name))
        }
        _ => None,
    }
}
