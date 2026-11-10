//! Every key binding, and the help overlay that lists them. Both come
//! from [`KEYMAP`], so a key can't be bound without being listed or
//! listed without being bound.

use gpui::{App, Div, KeyBinding, div, prelude::*, px};

use crate::boxes::proc::{
    Cancel, ClearFilter, CommitFilter, Confirm, Kill, PageDown, PageUp, ReverseSort, SelectFirst,
    SelectLast, SelectNext, SelectPrevious, SortNext, SortPrevious, StartFilter, Terminate,
    ToggleDetail, ToggleFold, ToggleTree,
};
use crate::monitor::{
    CloseHelp, FewerGraphSamples, LongerInterval, MoreGraphSamples, NextTheme, PreviousTheme, Quit,
    ShorterInterval, ShowHelp,
};
use crate::theme::Theme;
use crate::ui::{ROW_HEIGHT, chars, panel};

/// Key contexts. The process box sets the first three by its mode, and
/// the help overlay sets the last while it's open.
const BROWSE: Option<&str> = Some("Browse");
const FILTER: Option<&str> = Some("Filter");
const CONFIRM: Option<&str> = Some("Confirm");
pub const HELP: &str = "Help";

struct Section {
    title: &'static str,
    context: Option<&'static str>,
    keys: &'static [Key],
}

struct Key {
    /// gpui keystrokes, all bound to the same action.
    strokes: &'static [&'static str],
    /// How the overlay names the keys. Empty for a binding that another
    /// entry's text already covers, such as the second half of a pair.
    shown: &'static str,
    help: &'static str,
    bind: fn(&str, Option<&str>) -> KeyBinding,
}

const KEYMAP: &[Section] = &[
    Section {
        title: "Processes",
        context: BROWSE,
        keys: &[
            Key {
                strokes: &["j", "down"],
                shown: "j  ↓",
                help: "next process",
                bind: |key, context| KeyBinding::new(key, SelectNext, context),
            },
            Key {
                strokes: &["k", "up"],
                shown: "k  ↑",
                help: "previous process",
                bind: |key, context| KeyBinding::new(key, SelectPrevious, context),
            },
            Key {
                strokes: &["pagedown"],
                shown: "PgDn  PgUp",
                help: "a page down or up",
                bind: |key, context| KeyBinding::new(key, PageDown, context),
            },
            Key {
                strokes: &["pageup"],
                shown: "",
                help: "",
                bind: |key, context| KeyBinding::new(key, PageUp, context),
            },
            Key {
                strokes: &["home"],
                shown: "Home  End",
                help: "first or last process",
                bind: |key, context| KeyBinding::new(key, SelectFirst, context),
            },
            Key {
                strokes: &["end"],
                shown: "",
                help: "",
                bind: |key, context| KeyBinding::new(key, SelectLast, context),
            },
            Key {
                strokes: &["enter"],
                shown: "enter",
                help: "details of the selected process",
                bind: |key, context| KeyBinding::new(key, ToggleDetail, context),
            },
            Key {
                strokes: &["escape"],
                shown: "esc",
                help: "clear the filter, then close details",
                bind: |key, context| KeyBinding::new(key, ClearFilter, context),
            },
            Key {
                strokes: &["f", "/"],
                shown: "f  /",
                help: "filter by name, command, user or pid",
                bind: |key, context| KeyBinding::new(key, StartFilter, context),
            },
            Key {
                strokes: &["right"],
                shown: "←  →",
                help: "sort by the previous or next column",
                bind: |key, context| KeyBinding::new(key, SortNext, context),
            },
            Key {
                strokes: &["left"],
                shown: "",
                help: "",
                bind: |key, context| KeyBinding::new(key, SortPrevious, context),
            },
            Key {
                strokes: &["r"],
                shown: "r",
                help: "reverse the sort",
                bind: |key, context| KeyBinding::new(key, ReverseSort, context),
            },
            Key {
                strokes: &["e"],
                shown: "e",
                help: "tree view",
                bind: |key, context| KeyBinding::new(key, ToggleTree, context),
            },
            Key {
                strokes: &["space"],
                shown: "space",
                help: "fold or unfold a branch",
                bind: |key, context| KeyBinding::new(key, ToggleFold, context),
            },
            Key {
                strokes: &["t"],
                shown: "t",
                help: "send SIGTERM, after a prompt",
                bind: |key, context| KeyBinding::new(key, Terminate, context),
            },
            Key {
                strokes: &["shift-k"],
                shown: "K",
                help: "send SIGKILL, after a prompt",
                bind: |key, context| KeyBinding::new(key, Kill, context),
            },
        ],
    },
    Section {
        title: "Display",
        context: BROWSE,
        keys: &[
            // `+` arrives as `shift-=` or as `+` depending on the layout
            // and the platform's keymap handling; `=` saves reaching for
            // shift.
            Key {
                strokes: &["+", "shift-=", "="],
                shown: "+  -",
                help: "refresh processes less or more often",
                bind: |key, context| KeyBinding::new(key, LongerInterval, context),
            },
            Key {
                strokes: &["-"],
                shown: "",
                help: "",
                bind: |key, context| KeyBinding::new(key, ShorterInterval, context),
            },
            Key {
                strokes: &["]"],
                shown: "]  [",
                help: "more or fewer graph samples per refresh",
                bind: |key, context| KeyBinding::new(key, MoreGraphSamples, context),
            },
            Key {
                strokes: &["["],
                shown: "",
                help: "",
                bind: |key, context| KeyBinding::new(key, FewerGraphSamples, context),
            },
            Key {
                strokes: &["c"],
                shown: "c  C",
                help: "next or previous theme",
                bind: |key, context| KeyBinding::new(key, NextTheme, context),
            },
            Key {
                strokes: &["shift-c"],
                shown: "",
                help: "",
                bind: |key, context| KeyBinding::new(key, PreviousTheme, context),
            },
            Key {
                strokes: &["?", "shift-/"],
                shown: "?",
                help: "this help",
                bind: |key, context| KeyBinding::new(key, ShowHelp, context),
            },
            Key {
                strokes: &["q"],
                shown: "q",
                help: "quit",
                bind: |key, context| KeyBinding::new(key, Quit, context),
            },
        ],
    },
    Section {
        title: "While filtering",
        context: FILTER,
        keys: &[
            Key {
                strokes: &["down"],
                shown: "↓  ↑",
                help: "move through the matches",
                bind: |key, context| KeyBinding::new(key, SelectNext, context),
            },
            Key {
                strokes: &["up"],
                shown: "",
                help: "",
                bind: |key, context| KeyBinding::new(key, SelectPrevious, context),
            },
            Key {
                strokes: &["enter"],
                shown: "enter",
                help: "keep the filter and browse",
                bind: |key, context| KeyBinding::new(key, CommitFilter, context),
            },
            Key {
                strokes: &["escape"],
                shown: "esc",
                help: "drop the filter",
                bind: |key, context| KeyBinding::new(key, ClearFilter, context),
            },
        ],
    },
    Section {
        title: "Signal prompt",
        context: CONFIRM,
        keys: &[
            Key {
                strokes: &["y", "enter"],
                shown: "y  enter",
                help: "send it",
                bind: |key, context| KeyBinding::new(key, Confirm, context),
            },
            Key {
                strokes: &["n", "escape"],
                shown: "n  esc",
                help: "don't",
                bind: |key, context| KeyBinding::new(key, Cancel, context),
            },
        ],
    },
    Section {
        title: "Anywhere",
        context: None,
        keys: &[Key {
            strokes: &["ctrl-q"],
            shown: "ctrl-q",
            help: "quit, even while typing a filter",
            bind: |key, context| KeyBinding::new(key, Quit, context),
        }],
    },
    // Not listed: the overlay says how to close it.
    Section {
        title: "",
        context: Some(HELP),
        keys: &[Key {
            strokes: &["escape", "?", "shift-/", "q", "enter"],
            shown: "",
            help: "",
            bind: |key, context| KeyBinding::new(key, CloseHelp, context),
        }],
    },
];

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(KEYMAP.iter().flat_map(|section| {
        section.keys.iter().flat_map(move |key| {
            key.strokes
                .iter()
                .map(move |stroke| (key.bind)(stroke, section.context))
        })
    }));
}

/// The overlay's box, listing every section with a title.
pub fn help(theme: &Theme) -> Div {
    let sections = KEYMAP
        .iter()
        .filter(|section| !section.title.is_empty())
        .map(|section| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(ROW_HEIGHT))
                        .text_color(theme.title)
                        .child(section.title),
                )
                .children(
                    section
                        .keys
                        .iter()
                        .filter(|key| !key.shown.is_empty())
                        .map(|key| {
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .w(chars(11.0))
                                        .flex_shrink_0()
                                        .text_color(theme.highlight)
                                        .child(key.shown),
                                )
                                .child(div().text_color(theme.text).child(key.help))
                        }),
                )
        });
    panel(
        theme,
        "keys",
        theme.proc_box,
        Some("esc, ? or a click closes".into()),
    )
    .bg(theme.background)
    .w(chars(60.0))
    .gap_2()
    .children(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each keystroke means one thing in its context. gpui would
    /// otherwise pick one of the clashing bindings silently.
    #[test]
    fn no_keystroke_is_bound_twice_in_a_context() {
        let mut seen = std::collections::HashSet::new();
        for section in KEYMAP {
            for key in section.keys {
                for stroke in key.strokes {
                    assert!(
                        seen.insert((section.context, *stroke)),
                        "{stroke:?} is bound twice in {:?}",
                        section.context
                    );
                }
            }
        }
    }

    #[test]
    fn every_listed_key_says_what_it_does() {
        for section in KEYMAP {
            for key in section.keys {
                assert_eq!(
                    key.shown.is_empty(),
                    key.help.is_empty(),
                    "{:?} in {:?}",
                    key.strokes,
                    section.title
                );
            }
        }
    }
}
