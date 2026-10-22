//! The grants popover (`shell.md` section 10): what one session may do, changed in place.
//!
//! ```text
//!  designer (MCP over stdio)                        This session only
//!  [x] read              [x] edit:character     [x] edit:scene     [x] render
//!  [ ] broadcast         [ ] publish            [ ] spend  $0.00 of [ 2.00 ]
//!  [ ] net               [ ] fs: [ + add folder ]            [ ] native
//!  [ Revoke all ]                              [ Grant for this project ]
//! ```
//!
//! A checkbox per scope, never above the person's own grants. Granting `spend` needs a cap above $0 first. The widest
//! grant is said in words under the boxes. Turning on `broadcast`, `publish`, `spend` or `native` (any scope that
//! reaches outside the project) is not done here: it comes back as [`GrantChange::Risky`] for the caller's two-step
//! confirm. Everything else comes back as the new grants or budget, which the caller writes through
//! `Sessions::set_grants` and `Sessions::set_budget`. Nothing here writes anything itself.

use egui::{Align, Layout, RichText, Ui};
use omuzaki_ops_api::{Budget, Grants, Scope, SessionInfo};

use super::super::text::{self, say};
use super::approvals::dollars;
use crate::ds::theme::Ds;
use crate::ds::widgets::{Button, TextField, Toggle};

/// Width of the popover's content.
pub const WIDTH: f32 = 440.0;

/// What the popover keeps between frames for one session.
#[derive(Clone, Debug, Default)]
pub struct GrantsState {
    /// The spending cap as typed, in dollars.
    pub cap: String,
    /// The folder path being added.
    pub folder: String,
    /// The cap field was filled from the session.
    filled: bool,
}

impl GrantsState {
    /// The cap as typed, in micro-dollars, if it reads as an amount.
    pub fn cap_micros(&self) -> Option<u64> {
        parse_cap(&self.cap)
    }
}

/// What the person changed.
#[derive(Clone, Debug, PartialEq)]
pub enum GrantChange {
    /// The session's new grants.
    Grants(Grants),
    /// The session's new budget.
    Budget(Budget),
    /// The person turned on a scope that reaches outside the project; confirm before granting it.
    Risky(Scope),
    /// Remember these grants on the session's credential, for this project.
    Project(Grants),
}

/// The popover's body for `session`. `person` is the person's own grants (the ceiling); `project` offers "Grant for
/// this project" (only a session with a credential can remember a grant).
pub fn grants_ui(
    ui: &mut Ui,
    ds: &Ds,
    session: &SessionInfo,
    person: &Grants,
    state: &mut GrantsState,
    project: bool,
) -> Option<GrantChange> {
    let t = &ds.theme;
    if !state.filled {
        state.cap = cents(session.budget.usd_micros);
        state.filled = true;
    }
    let mut change = None;
    ui.set_width(WIDTH);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&session.client).font(ds.text.strong.clone()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(say(ds, text::GRANTS_SESSION_ONLY, &[])).font(ds.text.small.clone()).color(t.text2));
        });
    });
    let grants = &session.grants;
    let above = say(ds, text::GRANTS_ABOVE_YOU, &[]);
    let cap_first = say(ds, text::GRANTS_CAP_FIRST, &[]);
    let cap = parse_cap(&state.cap);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for scope in Scope::ALL {
            if scope == Scope::Spend || scope == Scope::Fs {
                continue;
            }
            if let Some(c) = scope_box(ui, grants, person, scope, &above, None) {
                change = Some(c);
            }
        }
    });
    // Spend, with its cap: granting it needs a cap above $0.
    ui.horizontal(|ui| {
        let blocked = (!grants.allows(Scope::Spend) && cap.is_none_or(|c| c == 0)).then_some(cap_first.as_ref());
        if let Some(c) = scope_box(ui, grants, person, Scope::Spend, &above, blocked) {
            change = Some(c);
        }
        let spent = dollars(session.spent.usd_micros);
        ui.label(RichText::new(say(ds, text::GRANTS_SPENT, &[("spent", &spent)])).color(t.text2));
        let label = say(ds, text::GRANTS_CAP, &[]);
        let field = ui.add(TextField::new(&label, &mut state.cap).width(72.0).id_salt(("grants_cap", session.id)));
        let entered = field.lost_focus() || (field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
        if entered
            && let Some(micros) = parse_cap(&state.cap)
            && micros != session.budget.usd_micros
        {
            change = Some(GrantChange::Budget(Budget { usd_micros: micros, ..session.budget }));
        }
    });
    // Files outside the project, by folder (ADR 0013).
    ui.horizontal_wrapped(|ui| {
        if let Some(c) = scope_box(ui, grants, person, Scope::Fs, &above, None) {
            change = Some(c);
        }
        for root in grants.fs_roots() {
            let name = say(ds, text::GRANTS_FS_REMOVE, &[("root", root)]);
            if ui.add(Button::new(root).ghost().icon(crate::ds::icons::X).accessible_label(&name)).clicked() {
                change = Some(GrantChange::Grants(without_root(grants, root)));
            }
        }
    });
    if grants.allows(Scope::Fs) || person.allows(Scope::Fs) {
        ui.horizontal(|ui| {
            let label = say(ds, text::GRANTS_FS_PATH, &[]);
            ui.add(TextField::new(&label, &mut state.folder).width(260.0).id_salt(("grants_folder", session.id)));
            let add = say(ds, text::GRANTS_FS_ADD, &[]);
            let empty = say(ds, text::GRANTS_FS_FIRST, &[]);
            let folder = state.folder.trim().to_owned();
            if ui.add(Button::new(&add).icon(crate::ds::icons::PLUS).disabled_if(folder.is_empty(), &empty)).clicked() {
                change = Some(GrantChange::Grants(grants.clone().with_fs_root(folder)));
                state.folder.clear();
            }
        });
    }
    let widest = widest(ds, session);
    ui.add(egui::Label::new(RichText::new(widest).color(t.text)).wrap());
    ui.horizontal(|ui| {
        if ui.add(Button::new(&say(ds, text::GRANTS_REVOKE_ALL, &[])).danger()).clicked() {
            change = Some(GrantChange::Grants(Grants::default()));
        }
        if project {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new(&say(ds, text::GRANTS_PROJECT, &[]))).clicked() {
                    change = Some(GrantChange::Project(grants.clone()));
                }
            });
        }
    });
    change
}

/// One scope's checkbox. Turning a risky scope on asks for the confirm instead of granting it.
fn scope_box(
    ui: &mut Ui,
    grants: &Grants,
    person: &Grants,
    scope: Scope,
    above: &str,
    blocked: Option<&str>,
) -> Option<GrantChange> {
    let held = grants.allows(scope);
    let mut value = held;
    let mut toggle = Toggle::checkbox(scope.name(), &mut value).id_salt(("grant", scope.name()));
    // A scope the person does not hold cannot be given; one already held can always be taken back.
    if !held && !person.allows(scope) {
        toggle = toggle.disabled(above);
    } else if let Some(reason) = blocked {
        toggle = toggle.disabled(reason);
    }
    let response = ui.add(toggle);
    if !response.changed() || value == held {
        return None;
    }
    Some(match (value, scope.is_risky()) {
        (true, true) => GrantChange::Risky(scope),
        (true, false) => GrantChange::Grants(grants.clone().with(scope)),
        (false, _) => GrantChange::Grants(grants.clone().without(scope)),
    })
}

/// `grants` without the file root `root`.
fn without_root(grants: &Grants, root: &str) -> Grants {
    let mut out = grants.clone().without(Scope::Fs);
    let mut any = false;
    for kept in grants.fs_roots().iter().filter(|r| r.as_str() != root) {
        out = out.with_fs_root(kept.clone());
        any = true;
    }
    if !any && grants.allows(Scope::Fs) { out.with(Scope::Fs) } else { out }
}

/// The widest thing the session may do, in a sentence ("designer can spend up to $2.00 on paid connectors").
pub fn widest(ds: &Ds, session: &SessionInfo) -> String {
    let g = &session.grants;
    let name = session.client.as_str();
    let cap = dollars(session.budget.usd_micros);
    let roots = g.fs_roots().len().to_string();
    let (line, extra): (&str, Option<(&str, &str)>) = if g.allows(Scope::Native) {
        (text::WIDEST_NATIVE, None)
    } else if g.allows(Scope::Spend) {
        (text::WIDEST_SPEND, Some(("cap", &cap)))
    } else if g.allows(Scope::Broadcast) {
        (text::WIDEST_BROADCAST, None)
    } else if g.allows(Scope::Publish) {
        (text::WIDEST_PUBLISH, None)
    } else if g.allows(Scope::Net) {
        (text::WIDEST_NET, None)
    } else if g.allows(Scope::Fs) {
        (text::WIDEST_FS, Some(("count", &roots)))
    } else if g.allows(Scope::EditCharacter) || g.allows(Scope::EditScene) || g.allows(Scope::EditLibrary) {
        (text::WIDEST_EDIT, None)
    } else if g.allows(Scope::Render) {
        (text::WIDEST_RENDER, None)
    } else if g.allows(Scope::Read) {
        (text::WIDEST_READ, None)
    } else {
        (text::WIDEST_NONE, None)
    };
    let mut args = vec![("name", name)];
    args.extend(extra);
    say(ds, line, &args).into_owned()
}

/// A dollar amount as typed ("2", "2.40", "$2.4"), in micro-dollars; `None` if it isn't one.
fn parse_cap(text: &str) -> Option<u64> {
    let text = text.trim().trim_start_matches('$');
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && frac.is_empty() || frac.len() > 6 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole: u64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let frac: u64 = if frac.is_empty() { 0 } else { format!("{frac:0<6}").parse().ok()? };
    whole.checked_mul(1_000_000)?.checked_add(frac)
}

/// Micro-dollars as the cap field shows them ("2.00").
fn cents(micros: u64) -> String {
    dollars(micros).trim_start_matches('$').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_parse_as_dollars() {
        assert_eq!(parse_cap("2"), Some(2_000_000));
        assert_eq!(parse_cap("$2.40"), Some(2_400_000));
        assert_eq!(parse_cap(" 0.5 "), Some(500_000));
        assert_eq!(parse_cap("abc"), None);
        assert_eq!(parse_cap("1.2345678"), None);
        assert_eq!(parse_cap(""), None);
        assert_eq!(cents(2_400_000), "2.40");
    }

    #[test]
    fn removing_one_root_keeps_the_others() {
        let grants = Grants::agent_default().with_fs_root("/kit/a").with_fs_root("/kit/b");
        let kept = without_root(&grants, "/kit/a");
        assert_eq!(kept.fs_roots(), ["/kit/b".to_owned()]);
        assert!(kept.allows(Scope::Fs) && kept.allows(Scope::Read));
    }
}
