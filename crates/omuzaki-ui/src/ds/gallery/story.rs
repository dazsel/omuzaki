//! A story: one component shown in all its states. Stories register themselves with [`register_story!`], so adding a
//! component never edits a shared list (spec v3 section 39 rule 2); the gallery lists whatever is compiled in.

use std::any::Any;

use egui::Ui;

/// Which part of the library a story belongs to; the gallery groups cards by it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Group {
    /// Colour, type, spacing: the tokens themselves.
    Foundations,
    /// Things you press, type into or drag.
    Controls,
    /// Chips, rows and cards that show state.
    Display,
    /// Things that appear over or beside the work: toasts, approvals.
    Feedback,
    /// Whole panels assembled from the components, to judge how they sit together.
    Composites,
}

impl Group {
    /// The heading shown above the group.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Foundations => "Foundations",
            Self::Controls => "Controls",
            Self::Display => "Display",
            Self::Feedback => "Feedback",
            Self::Composites => "Composites",
        }
    }
}

/// One registered story. Build it with [`register_story!`].
pub struct Story {
    /// Stable id: the key of its state and the name in screenshot file names.
    pub id: &'static str,
    /// The card heading.
    pub title: &'static str,
    /// The section it appears in.
    pub group: Group,
    /// Order within the group, lowest first.
    pub order: u32,
    /// Makes the story's state (the values its widgets edit) once.
    pub new_state: fn() -> Box<dyn Any>,
    /// Draws the story into a card body.
    pub show: fn(&mut Ui, &mut dyn Any),
}

inventory::collect!(Story);

/// Every registered story, grouped and ordered.
pub fn all() -> Vec<&'static Story> {
    let mut stories: Vec<&'static Story> = inventory::iter::<Story>.into_iter().collect();
    stories.sort_by_key(|s| (s.group, s.order, s.id));
    stories
}

/// Registers a story:
///
/// ```ignore
/// register_story! {
///     id: "button", title: "Button", group: Group::Controls, order: 10,
///     state: ButtonState,                       // `Default`; what the widgets edit
///     show: |ui, state| { /* draw */ },
/// }
/// ```
#[macro_export]
macro_rules! register_story {
    (id: $id:literal, title: $title:literal, group: $group:expr, order: $order:literal, state: $state:ty, show: $show:expr $(,)?) => {
        ::inventory::submit! {
            $crate::ds::gallery::Story {
                id: $id,
                title: $title,
                group: $group,
                order: $order,
                new_state: || ::std::boxed::Box::new(<$state as ::std::default::Default>::default()),
                show: |ui, state| {
                    if let Some(state) = state.downcast_mut::<$state>() {
                        let show: fn(&mut ::egui::Ui, &mut $state) = $show;
                        show(ui, state);
                    }
                },
            }
        }
        $crate::__record_registration!("story", $id);
    };
}
