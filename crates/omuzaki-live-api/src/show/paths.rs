//! Where shows live: in the document tree, as typed [`DocPath`]s that ops build patches against, and on disk, for the
//! machine-local state that is never in the document (design D10, D11). Nothing else spells these paths by hand.

use std::path::{Path, PathBuf};

use omuzaki_core::id::Slug;
use omuzaki_doc_api::DocPath;

use crate::ids::{GraphId, NodeId, ShowId};

/// `shows/<id>`: the whole [`ShowDef`](super::ShowDef).
#[must_use]
pub fn show(id: &ShowId) -> DocPath {
    DocPath::root().join("shows").join(id.as_str())
}

/// `shows/<id>/show`: the show's settings and declarations (`show.toml`).
#[must_use]
pub fn settings(id: &ShowId) -> DocPath {
    show(id).join("show")
}

/// `shows/<id>/automations`: the graphs.
#[must_use]
pub fn automations(id: &ShowId) -> DocPath {
    show(id).join("automations")
}

/// `shows/<show>/automations/<graph>`: one [`GraphDef`](crate::graph::GraphDef).
#[must_use]
pub fn graph(show: &ShowId, graph: &GraphId) -> DocPath {
    automations(show).join(graph.as_str())
}

/// `shows/<show>/automations/<graph>/graph`: the graph's settings (`graph.toml`).
#[must_use]
pub fn graph_settings(show: &ShowId, graph_id: &GraphId) -> DocPath {
    graph(show, graph_id).join("graph")
}

/// `shows/<show>/automations/<graph>/nodes/<node>`: one [`NodeDef`](crate::graph::NodeDef).
#[must_use]
pub fn node(show: &ShowId, graph_id: &GraphId, node: &NodeId) -> DocPath {
    graph(show, graph_id).join("nodes").join(node.as_str())
}

/// `shows/<id>/applied/<app>`: the library's record of an application (ADR 0019), by slot or item name.
#[must_use]
pub fn applied(id: &ShowId, app: &Slug) -> DocPath {
    show(id).join("applied").join(app.as_str())
}

/// `project/default_show`: the id of the show the project runs and edits by default.
#[must_use]
pub fn default_show() -> DocPath {
    DocPath::root().join("project").join("default_show")
}

/// `<project>/.omuzaki/automation/<show>/values.toml`: the show-scope variable values on this machine, beside the op
/// log and never committed.
#[must_use]
pub fn values_file(project_root: &Path, show: &ShowId) -> PathBuf {
    project_root.join(".omuzaki").join("automation").join(show.as_str()).join("values.toml")
}

/// `<data>/automation/globals.toml`: the global variable values of this OS user. `data_dir` is Omuzaki's own data
/// folder.
#[must_use]
pub fn globals_file(data_dir: &Path) -> PathBuf {
    data_dir.join("automation").join("globals.toml")
}

/// `<data>/automation/approvals.toml`: what the person approved, per project folder and show, on this machine.
/// `data_dir` is Omuzaki's own data folder. Never in a project, so a shared project carries no approvals.
#[must_use]
pub fn approvals_file(data_dir: &Path) -> PathBuf {
    data_dir.join("automation").join("approvals.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_documented_layout() {
        let (s, g, n) = (ShowId::new("main").unwrap(), GraphId::new("bonk").unwrap(), NodeId::new("cool").unwrap());
        assert_eq!(show(&s).to_string(), "shows/main");
        assert_eq!(settings(&s).to_string(), "shows/main/show");
        assert_eq!(graph_settings(&s, &g).to_string(), "shows/main/automations/bonk/graph");
        assert_eq!(node(&s, &g, &n).to_string(), "shows/main/automations/bonk/nodes/cool");
        assert_eq!(applied(&s, &Slug::new("bonk").unwrap()).to_string(), "shows/main/applied/bonk");
        assert_eq!(default_show().to_string(), "project/default_show");
        let root = Path::new("p");
        assert_eq!(values_file(root, &s), Path::new("p/.omuzaki/automation/main/values.toml"));
        assert_eq!(globals_file(root), Path::new("p/automation/globals.toml"));
        assert_eq!(approvals_file(root), Path::new("p/automation/approvals.toml"));
    }
}
