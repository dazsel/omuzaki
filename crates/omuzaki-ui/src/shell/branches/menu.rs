//! The branch chip and menu (`docs/design/branches.md` 5.4 to 5.6, 8.3 and 9; #440).
//!
//! The chip in the status bar names the branch the project folder shows, and how many branches wait for review. It
//! opens the menu: the folder branch, the review cards of agents' ready work (Compare, Merge, Send back with a
//! comment), the person's branches, agents' branches, the ones Omuzaki kept (a restore's backup) and the bin. Each row
//! switches, compares, merges into the folder branch, renames and deletes; the bin's rows restore. New branch makes
//! one from the folder branch as it is now.
//!
//! Everything goes through the app core's lane manager as the person; a refusal (an open transaction, a file changed
//! outside Omuzaki, a taken name) is said in the menu with its hint, and nothing changes.

use egui::{Align2, Context, Id, Key, Modifiers, Response, RichText, Ui};
use omuzaki_history_api::branch::{
    BranchError, BranchId, BranchInfo, BranchName, LaneState, NewBranch, ReviewDecision, ReviewState,
};
use omuzaki_ops_api::Actor;

use super::{Asked, Branching, Listing, person};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{Button, TextField};
use crate::shell::text::{Word, say};

/// The menu's state between frames.
#[derive(Debug, Default)]
pub struct Menu {
    open: bool,
    listing: Listing,
    /// The new-branch form: its name and title, while it is open.
    new: Option<(String, String)>,
    /// The branch being renamed and its new name.
    renaming: Option<(BranchId, String)>,
    /// A switch waiting for the person to confirm that agents keep working on the folder branch.
    confirming: Option<BranchId>,
    /// The review being sent back and its comment.
    sending_back: Option<(BranchId, String)>,
    /// What the last action was refused for, with its hint.
    problem: Option<String>,
}

impl Menu {
    /// Whether the menu is open.
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Opens or closes the menu.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.listing.refresh();
    }

    /// The branches as the menu last read them.
    pub fn listing(&mut self, branching: &Branching) -> &Listing {
        self.listing.follow(branching);
        &self.listing
    }

    /// The chip: the folder branch's name, and how many wait for review. A click opens the menu.
    pub fn chip(&mut self, ui: &mut Ui, branching: &Branching) -> Response {
        self.listing.follow(branching);
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        let name = self.listing.folder_branch().map_or_else(String::new, |b| b.name.to_string());
        let ready = self.listing.ready().count();
        let text = if ready == 0 {
            say(&ctx, Word::BranchChip, &[("branch", &name)])
        } else {
            say(&ctx, Word::BranchChipReady, &[("branch", &name), ("count", &ready.to_string())])
        };
        let colour = if ready == 0 { ds.theme.text2 } else { ds.theme.accent };
        let label = RichText::new(format!("{} {text}", icons::GIT_BRANCH)).font(ds.text.small.clone()).color(colour);
        let response = ui.add(egui::Button::new(label).frame(false)).on_hover_text(say(&ctx, Word::BranchMenu, &[]));
        if response.clicked() {
            self.toggle();
        }
        response
    }

    /// Draws the menu while it is open; what it asks the shell to do.
    pub fn show(&mut self, ctx: &Context, branching: &Branching) -> Option<Asked> {
        if !self.open {
            return None;
        }
        self.listing.follow(branching);
        let ds = ds(ctx);
        let mut asked = None;
        let mut open = true;
        egui::Window::new(say(ctx, Word::BranchMenu, &[]))
            .id(Id::new("branch_menu"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::LEFT_BOTTOM, [8.0, -32.0])
            .default_width(380.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                    asked = self.body(ui, &ds, branching);
                });
            });
        let escape = !ctx.text_edit_focused() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        if !open || escape || asked.is_some() {
            self.open = false;
            self.new = None;
            self.renaming = None;
            self.confirming = None;
            self.sending_back = None;
        }
        if asked.is_some() {
            self.problem = None;
        }
        asked
    }

    fn body(&mut self, ui: &mut Ui, ds: &Ds, branching: &Branching) -> Option<Asked> {
        let ctx = ui.ctx().clone();
        let Some(folder) = self.listing.folder_branch().cloned() else { return None };
        let mut asked = None;
        ui.label(RichText::new(say(&ctx, Word::BranchOn, &[("branch", folder.name.as_str())])).strong());
        ui.label(meta(&ctx, ds, &folder));
        if let Some(problem) = &self.problem {
            ui.add_space(4.0);
            ui.label(RichText::new(problem).color(ds.theme.danger));
        }
        ui.add_space(6.0);
        self.new_branch(ui, ds, branching, &folder);

        let live: Vec<BranchInfo> =
            self.listing.branches.iter().filter(|b| b.deleted.is_none() && b.id != folder.id).cloned().collect();
        if live.is_empty() && self.new.is_none() {
            ui.label(RichText::new(say(&ctx, Word::BranchAlone, &[("branch", folder.name.as_str())])).color(ds.theme.text2));
        }

        let ready: Vec<BranchInfo> = self.listing.ready().cloned().collect();
        if !ready.is_empty() {
            heading(ui, ds, &say(&ctx, Word::BranchGroupReady, &[]));
            for branch in &ready {
                if let Some(a) = self.review_card(ui, ds, branching, branch, &folder) {
                    asked = Some(a);
                }
            }
        }
        let groups = [
            (Word::BranchGroupYours, live.iter().filter(|b| !b.auto && matches!(b.created_by, Actor::Person { .. })).collect::<Vec<_>>()),
            (Word::BranchGroupAgents, live.iter().filter(|b| !b.auto && !matches!(b.created_by, Actor::Person { .. })).collect()),
            (Word::BranchGroupAuto, live.iter().filter(|b| b.auto).collect()),
        ];
        for (word, branches) in groups {
            if branches.is_empty() {
                continue;
            }
            heading(ui, ds, &say(&ctx, word, &[]));
            for branch in branches {
                if let Some(a) = self.row(ui, ds, branching, branch, &folder) {
                    asked = Some(a);
                }
            }
        }
        let bin: Vec<BranchInfo> = self.listing.branches.iter().filter(|b| b.deleted.is_some()).cloned().collect();
        if !bin.is_empty() {
            heading(ui, ds, &say(&ctx, Word::BranchGroupBin, &[]));
            for branch in &bin {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(branch.name.as_str()).color(ds.theme.text2));
                    let restore = say(&ctx, Word::BranchRestore, &[]);
                    if ui.add(Button::new(&restore).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE)).clicked() {
                        self.outcome(branching.branches.undelete(&branch.id, &person()).map(|_| ()));
                    }
                });
            }
        }
        asked
    }

    /// New branch, and its form while open.
    fn new_branch(&mut self, ui: &mut Ui, ds: &Ds, branching: &Branching, folder: &BranchInfo) {
        let ctx = ui.ctx().clone();
        let Some((name, title)) = &mut self.new else {
            if ui.add(Button::new(&say(&ctx, Word::BranchNew, &[])).icon(icons::PLUS)).clicked() {
                self.new = Some((String::new(), String::new()));
            }
            return;
        };
        ui.add(TextField::new(&say(&ctx, Word::BranchName, &[]), name).id_salt("branch_new_name"));
        ui.add(TextField::new(&say(&ctx, Word::BranchTitle, &[]), title).id_salt("branch_new_title"));
        ui.label(
            RichText::new(say(&ctx, Word::BranchNewFrom, &[("branch", folder.name.as_str())]))
                .font(ds.text.small.clone())
                .color(ds.theme.text2),
        );
        let (mut make, mut cancel) = (false, false);
        ui.horizontal(|ui| {
            let label = say(&ctx, Word::BranchMake, &[]);
            make = ui.add(Button::new(&label).primary().disabled_if(name.trim().is_empty(), &label)).clicked();
            cancel = ui.add(Button::new(&say(&ctx, Word::BranchCancel, &[])).ghost()).clicked();
        });
        if cancel {
            self.new = None;
        } else if make {
            let (name, title) = (name.trim().to_owned(), title.trim().to_owned());
            let made = BranchName::new(name).map_err(|e| e.to_string()).and_then(|name| {
                let mut new = NewBranch::new(name, folder.id.clone());
                if !title.is_empty() {
                    new = new.with_title(title);
                }
                branching.branches.create(new, &person()).map_err(|e| refusal(&e))
            });
            match made {
                Ok(_) => {
                    self.new = None;
                    self.problem = None;
                    self.listing.refresh();
                }
                Err(why) => self.problem = Some(why),
            }
        }
    }

    /// A review card: who asked, the note, and Compare, Merge, Send back.
    fn review_card(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        branching: &Branching,
        branch: &BranchInfo,
        folder: &BranchInfo,
    ) -> Option<Asked> {
        let ctx = ui.ctx().clone();
        let review = branch.review.as_ref()?;
        let mut asked = None;
        egui::Frame::new().fill(ds.theme.bg2).inner_margin(8.0).corner_radius(6.0).show(ui, |ui| {
            let who = who(&review.by);
            ui.label(RichText::new(say(&ctx, Word::BranchReady, &[("who", &who), ("branch", branch.name.as_str())])).strong());
            if let Some(note) = &review.note {
                ui.label(RichText::new(say(&ctx, Word::BranchNote, &[("note", note)])).color(ds.theme.text2));
            }
            if let Some((id, comment)) = &mut self.sending_back
                && *id == branch.id
            {
                ui.add(TextField::new(&say(&ctx, Word::BranchSendBackWhat, &[]), comment).id_salt(("send_back", id.as_str())));
                let (mut send, mut cancel) = (false, false);
                ui.horizontal(|ui| {
                    let label = say(&ctx, Word::BranchSendBack, &[]);
                    send = ui.add(Button::new(&label).primary().disabled_if(comment.trim().is_empty(), &label)).clicked();
                    cancel = ui.add(Button::new(&say(&ctx, Word::BranchCancel, &[])).ghost()).clicked();
                });
                if send {
                    let decision = ReviewDecision::send_back(comment.trim());
                    let id = id.clone();
                    self.sending_back = None;
                    self.outcome(branching.branches.review(&id, decision, &person()).map(|_| ()));
                } else if cancel {
                    self.sending_back = None;
                }
                return;
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add(Button::new(&say(&ctx, Word::BranchCompare, &[])).icon(icons::COLUMNS)).clicked() {
                    asked = Some(Asked::Compare(vec![folder.id.clone(), branch.id.clone()]));
                }
                let merge = say(&ctx, Word::BranchMergeInto, &[("branch", folder.name.as_str())]);
                if ui.add(Button::new(&merge).primary().icon(icons::GIT_MERGE)).clicked() {
                    asked = Some(Asked::Merge(branch.id.clone(), Some(review.point)));
                }
                if ui.add(Button::new(&say(&ctx, Word::BranchSendBack, &[])).ghost()).clicked() {
                    self.sending_back = Some((branch.id.clone(), String::new()));
                }
            });
        });
        ui.add_space(4.0);
        asked
    }

    /// A branch's row: its name, title and state, and what can be done with it.
    fn row(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        branching: &Branching,
        branch: &BranchInfo,
        folder: &BranchInfo,
    ) -> Option<Asked> {
        let ctx = ui.ctx().clone();
        let mut asked = None;
        if let Some((id, name)) = &mut self.renaming
            && *id == branch.id
        {
            ui.add(TextField::new(&say(&ctx, Word::BranchName, &[]), name).id_salt(("rename", id.as_str())));
            let (mut save, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                save = ui.add(Button::new(&say(&ctx, Word::BranchSave, &[])).primary()).clicked();
                cancel = ui.add(Button::new(&say(&ctx, Word::BranchCancel, &[])).ghost()).clicked();
            });
            if save {
                let (id, name) = (id.clone(), name.trim().to_owned());
                let renamed = BranchName::new(name).map_err(|e| e.to_string()).and_then(|name| {
                    branching.branches.rename(&id, name, branch.title.clone(), &person()).map(|_| ()).map_err(|e| refusal(&e))
                });
                self.renaming = None;
                match renamed {
                    Ok(()) => {
                        self.problem = None;
                        self.listing.refresh();
                    }
                    Err(why) => self.problem = Some(why),
                }
            } else if cancel {
                self.renaming = None;
            }
            return None;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(branch.name.as_str()).strong());
            if let Some(title) = &branch.title {
                ui.label(RichText::new(title).color(ds.theme.text2));
            }
        });
        ui.label(meta(&ctx, ds, branch));
        if self.confirming.as_ref() == Some(&branch.id) {
            let sessions = match folder.lane {
                LaneState::Open { sessions, .. } => sessions,
                _ => 0,
            };
            let line = say(&ctx, Word::BranchSwitchAgents, &[("count", &sessions.to_string()), ("branch", folder.name.as_str())]);
            ui.label(RichText::new(line).color(ds.theme.warn));
            let (mut go, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                let label = say(&ctx, Word::BranchSwitchAnyway, &[("branch", branch.name.as_str())]);
                go = ui.add(Button::new(&label).primary()).clicked();
                cancel = ui.add(Button::new(&say(&ctx, Word::BranchCancel, &[])).ghost()).clicked();
            });
            if go {
                self.confirming = None;
                asked = self.switch(branching, &branch.id);
            } else if cancel {
                self.confirming = None;
            }
            return asked;
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new(&say(&ctx, Word::BranchSwitch, &[])).icon(icons::GIT_BRANCH)).clicked() {
                let agents = matches!(folder.lane, LaneState::Open { sessions, .. } if sessions > 0);
                if agents {
                    self.confirming = Some(branch.id.clone());
                } else {
                    asked = self.switch(branching, &branch.id);
                }
            }
            if ui.add(Button::new(&say(&ctx, Word::BranchCompare, &[])).ghost().icon(icons::COLUMNS)).clicked() {
                asked = Some(Asked::Compare(vec![folder.id.clone(), branch.id.clone()]));
            }
            let merge = say(&ctx, Word::BranchMergeInto, &[("branch", folder.name.as_str())]);
            if ui.add(Button::new(&merge).ghost().icon(icons::GIT_MERGE)).clicked() {
                asked = Some(Asked::Merge(branch.id.clone(), None));
            }
            if ui.add(Button::new(&say(&ctx, Word::BranchRename, &[])).ghost()).clicked() {
                self.renaming = Some((branch.id.clone(), branch.name.to_string()));
            }
            if ui.add(Button::new(&say(&ctx, Word::BranchDelete, &[])).ghost().icon(icons::TRASH)).clicked() {
                self.outcome(branching.branches.delete(&branch.id, &person()));
            }
        });
        ui.add_space(6.0);
        asked
    }

    fn switch(&mut self, branching: &Branching, to: &BranchId) -> Option<Asked> {
        match branching.branches.switch(to, &person()) {
            Ok(_) => {
                self.problem = None;
                self.listing.refresh();
                Some(Asked::Switched)
            }
            Err(error) => {
                self.problem = Some(refusal(&error));
                None
            }
        }
    }

    fn outcome(&mut self, result: Result<(), BranchError>) {
        match result {
            Ok(()) => {
                self.problem = None;
                self.listing.refresh();
            }
            Err(error) => self.problem = Some(refusal(&error)),
        }
    }
}

/// A refusal in words, with what to do about it.
fn refusal(error: &BranchError) -> String {
    match error.hint() {
        Some(hint) => format!("{error}. {hint}"),
        None => error.to_string(),
    }
}

/// Who asked, as the card names them.
fn who(actor: &Actor) -> String {
    match actor {
        Actor::Person { name } => name.clone(),
        Actor::Agent { id, .. } => id.split('#').next().unwrap_or(id).to_owned(),
        other => format!("{other:?}"),
    }
}

/// "version 3 · in the project folder · 2 agents working", and the review's state once decided.
fn meta(ctx: &Context, ds: &Ds, branch: &BranchInfo) -> RichText {
    let mut parts = vec![say(ctx, Word::BranchVersion, &[("seq", &branch.seq.to_string())])];
    if let LaneState::Open { folder, sessions, .. } = branch.lane {
        if folder {
            parts.push(say(ctx, Word::BranchHere, &[]));
        }
        if sessions > 0 {
            parts.push(say(ctx, Word::BranchAgentsOn, &[("count", &sessions.to_string())]));
        }
    }
    match branch.review.as_ref().map(|r| r.state) {
        Some(ReviewState::SentBack) => parts.push(say(ctx, Word::BranchSentBack, &[])),
        Some(ReviewState::Merged) => parts.push(say(ctx, Word::BranchMerged, &[])),
        _ => {}
    }
    RichText::new(parts.join(" · ")).font(ds.text.small.clone()).color(ds.theme.text2)
}

fn heading(ui: &mut Ui, ds: &Ds, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text).font(ds.text.small.clone()).color(ds.theme.text3).strong());
    ui.separator();
}
