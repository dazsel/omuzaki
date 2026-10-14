//! The two registration macros, `messages!` and `register_locales!`, and the helper macros they expand to.

/// Declares a crate's sentences: errors, findings, reasons and any other message with arguments (ADR 0023 decision
/// 3, design 3.2).
///
/// ```
/// omuzaki_l10n_api::messages! {
///     /// An op named a part the character doesn't have.
///     pub part_not_found = "char-part-not-found" (part: Id, character: Id)
///         => "{ $character } has no part { $part }.";
///     /// A key legend; the pseudo-locale leaves it alone.
///     #[verbatim]
///     pub key_enter = "ui-key-enter" () => "Enter";
/// }
///
/// let text = part_not_found("hair-b", "mira");
/// assert_eq!(text.id, "char-part-not-found");
/// assert_eq!(key_enter().id, "ui-key-enter");
/// ```
///
/// - The `///` lines are the translator note; the crate check requires one.
/// - `#[verbatim]` marks a key legend or format hint that is not prose.
/// - Each message becomes a `pub fn` returning a [`Text`](crate::Text) with typed parameters: `Id` and `Verbatim`
///   take `impl Into<String>`, `Int` an `i64`, `Num` an `f64`, `Qty` an `(f64, Unit)`, `Text` a `Text` and `List` a
///   `Vec<Arg>`. A missing argument, an extra one or one of the wrong kind is a compile error, not a `{$part}` on
///   screen:
///
/// ```compile_fail
/// omuzaki_l10n_api::messages! {
///     /// Names a part.
///     pub part_not_found = "x-part-not-found" (part: Id) => "No part { $part }.";
/// }
/// let _ = part_not_found("hair", "an argument the message does not declare");
/// ```
///
/// ```compile_fail
/// omuzaki_l10n_api::messages! {
///     /// Counts parts.
///     pub parts_removed = "x-parts-removed" (count: Int) => "Removed { $count }.";
/// }
/// let _ = parts_removed("three");
/// ```
///
/// - The table is submitted to `inventory` as a [`MessageTable`](crate::MessageTable) with the invoking package, and
///   recorded for the link gate (ADR 0015).
#[macro_export]
macro_rules! messages {
    ($(
        $(#[$($attr:tt)*])*
        $vis:vis $name:ident = $id:literal ( $($arg:ident : $kind:ident),* $(,)? ) => $english:literal;
    )+) => {
        $(
            #[doc = concat!(
                $($crate::__attr_note!($($attr)*),)*
                "\n\nMessage `", $id, "`: ", $english
            )]
            #[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
            #[must_use]
            $vis fn $name($($arg: $crate::__param_ty!($kind)),*) -> $crate::Text {
                $crate::Text {
                    id: ::std::borrow::Cow::Borrowed($id),
                    args: ::std::vec![$((
                        ::std::borrow::Cow::Borrowed(::core::stringify!($arg)),
                        $crate::__arg_value!($kind, $arg),
                    )),*],
                }
            }
        )+

        $crate::__inventory::submit! {
            $crate::MessageTable {
                package: ::core::env!("CARGO_PKG_NAME"),
                messages: &[$(
                    $crate::MessageDecl {
                        id: $id,
                        english: $english,
                        note: ::core::concat!($($crate::__attr_note!($($attr)*), "\n",)*),
                        args: &[$((::core::stringify!($arg), $crate::ArgKind::$kind)),*],
                        verbatim: false $(|| $crate::__attr_is_verbatim!($($attr)*))*,
                    },
                )+],
            }
        }
        $crate::__record_registration!("messages", messages);
    };
}

/// The text of one doc attribute, or nothing for `verbatim`.
#[doc(hidden)]
#[macro_export]
macro_rules! __attr_note {
    (doc = $text:literal) => {
        $text
    };
    (verbatim) => {
        ""
    };
}

/// Whether one attribute of a `messages!` entry is `verbatim`.
#[doc(hidden)]
#[macro_export]
macro_rules! __attr_is_verbatim {
    (verbatim) => {
        true
    };
    (doc = $text:literal) => {
        false
    };
}

/// The parameter type of an argument kind.
#[doc(hidden)]
#[macro_export]
macro_rules! __param_ty {
    (Id) => { impl ::core::convert::Into<::std::string::String> };
    (Verbatim) => { impl ::core::convert::Into<::std::string::String> };
    (Int) => { i64 };
    (Num) => { f64 };
    (Qty) => { (f64, $crate::Unit) };
    (Text) => { $crate::Text };
    (List) => { ::std::vec::Vec<$crate::Arg> };
}

/// An argument value as the `Arg` for its kind.
#[doc(hidden)]
#[macro_export]
macro_rules! __arg_value {
    (Id, $v:ident) => {
        $crate::Arg::Id($v.into())
    };
    (Verbatim, $v:ident) => {
        $crate::Arg::Verbatim($v.into())
    };
    (Int, $v:ident) => {
        $crate::Arg::Int($v)
    };
    (Num, $v:ident) => {
        $crate::Arg::num_or_verbatim($v)
    };
    (Qty, $v:ident) => {
        $crate::Arg::qty_or_verbatim($v.0, $v.1)
    };
    (Text, $v:ident) => {
        $crate::Arg::Text(::std::boxed::Box::new($v))
    };
    (List, $v:ident) => {
        $crate::Arg::List($v)
    };
}

/// Embeds this crate's Fluent files and registers them (ADR 0023 decision 4; §39 rule 2). Call it once in `lib.rs`.
///
/// It `include_str!`s `locales/<lang>/<CARGO_PKG_NAME>.ftl` for English and every shipped language
/// ([`SHIPPED`](crate::SHIPPED), plus [`OVERRIDES`](crate::OVERRIDES)), submits each as a
/// [`Resource`](crate::Resource), and records the registration for the link gate. A shipped language without its file
/// is a compile error; an override's file may be empty, since overrides are sparse.
#[macro_export]
macro_rules! register_locales {
    () => {
        $crate::__register_locale_files!("en", "ja", "ko", "zh-Hans", "es-419", "pt-BR");
        $crate::__record_registration!("locales", locales);
    };
}

/// Submits one `Resource` per language directory.
#[doc(hidden)]
#[macro_export]
macro_rules! __register_locale_files {
    ($($lang:literal),+) => {
        $($crate::__inventory::submit! {
            $crate::Resource {
                package: ::core::env!("CARGO_PKG_NAME"),
                locale: $lang,
                source: ::core::include_str!(::core::concat!(
                    ::core::env!("CARGO_MANIFEST_DIR"),
                    "/locales/", $lang, "/", ::core::env!("CARGO_PKG_NAME"), ".ftl"
                )),
            }
        })+
    };
}
