//! Translating keystrokes into actions, with vim's semantics.
//!
//! This module is **pure**: it knows neither ratatui, nor the engine, nor the
//! application state. It takes the current mode and a key, and returns what to
//! do. That is why the whole ergonomics — counts, pending operators, `gg`,
//! `<C-w>l`, the leader — can be tested without raising a terminal, which is
//! what makes adjusting it viable without fear.
//!
//! # The state it does keep
//!
//! Only what vim calls *pending input*: the count being typed (the `3` of
//! `3j`) and the half-typed operator (the `g` of `gg`). The mode is supplied
//! by the caller, because it belongs to the application and not to the
//! keyboard.
//!
//! # `Ctrl-Enter`
//!
//! Runs the request from any editing mode. Whether it arrives as such depends
//! on the terminal: without Kitty's keyboard protocol, `Ctrl-Enter` is
//! indistinguishable from `Enter` and behaves like it. See [`crate::runner`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The active editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Navigation and commands.
    #[default]
    Normal,
    /// Inserting text in the editor.
    Insert,
    /// Selecting by lines.
    Visual,
    /// The command line (`:`).
    Command,
    /// Search (`/`).
    Search,
}

impl Mode {
    /// The label drawn in the status line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::Command => "COMMAND",
            Self::Search => "SEARCH",
        }
    }
}

/// The focused pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// The collection tree.
    #[default]
    Tree,
    /// The request editor.
    Editor,
    /// The response.
    Response,
}

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One line down (`j`).
    Down,
    /// One line up (`k`).
    Up,
    /// One character left (`h`).
    Left,
    /// One character right (`l`).
    Right,
    /// The first line (`gg`).
    FirstLine,
    /// The last line (`G`).
    LastLine,
    /// A specific line (`{n}G`).
    ToLine(usize),
    /// The start of the line (`0`).
    LineStart,
    /// The end of the line (`$`).
    LineEnd,
    /// The next word (`w`).
    WordForward,
    /// The previous word (`b`).
    WordBack,
    /// Half a page down (`Ctrl-d`).
    HalfPageDown,
    /// Half a page up (`Ctrl-u`).
    HalfPageUp,
}

/// What the application must do as a consequence of a keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Move the cursor, repeated `count` times.
    Motion(Motion, usize),
    /// Focus the next or the previous pane.
    CyclePane(bool),
    /// Focus one specific pane.
    Focus(Pane),
    /// Change mode.
    SetMode(Mode),
    /// Enter insert mode one position to the right (`a`).
    InsertAfter,
    /// Open a line below and enter insert mode (`o`).
    OpenLineBelow,
    /// Open what is selected in the tree, or run if the editor has focus.
    Confirm,
    /// Run the request.
    Run,
    /// Preview without sending.
    Preview,
    /// Cancel the execution in flight.
    Cancel,
    /// Save the edited block.
    Save,
    /// Show or hide the variables pane.
    ToggleVars,
    /// Show or hide the pane with the resolved request.
    ToggleRequest,
    /// Move to the next environment.
    CycleEnv,
    /// Repeat the search forwards (`true`) or backwards.
    SearchAgain(bool),
    /// Type a character (insert mode or the command line).
    Char(char),
    /// Delete the preceding character.
    Backspace,
    /// Split the line in insert mode.
    Newline,
    /// Run the command line or the search.
    Submit,
    /// Cancel the current mode.
    Escape,
    /// Quit the application.
    Quit,
    /// Show the help.
    Help,
    /// Nothing to do.
    Nothing,
}

/// A half-typed operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Pending {
    /// Nothing pending.
    #[default]
    None,
    /// `g` was pressed and the second key is missing.
    G,
    /// `Ctrl-w` was pressed and the direction is missing.
    Window,
    /// The leader key was pressed and the action is missing.
    Leader,
}

/// The keyboard's state between keystrokes.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    count: String,
    pending: Pending,
}

impl Keymap {
    /// Creates a keymap with nothing pending.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The text of what is half-typed, for the status line.
    ///
    /// It is the equivalent of vim's bottom-right corner: showing the `3` of a
    /// half-typed `3j` saves the user wondering whether the key arrived.
    #[must_use]
    pub fn hint(&self) -> String {
        let operator = match self.pending {
            Pending::None => "",
            Pending::G => "g",
            Pending::Window => "^W",
            Pending::Leader => "␣",
        };
        format!("{}{operator}", self.count)
    }

    /// Discards the pending count and operator.
    pub fn reset(&mut self) {
        self.count.clear();
        self.pending = Pending::None;
    }

    /// Translates a keystroke in the given mode.
    #[must_use]
    pub fn feed(&mut self, mode: Mode, key: KeyEvent) -> Action {
        match mode {
            Mode::Command | Mode::Search => Self::feed_line(key),
            Mode::Insert => Self::feed_insert(key),
            Mode::Normal | Mode::Visual => self.feed_normal(key),
        }
    }

    /// Command-line and search modes: nothing but editing a string.
    fn feed_line(key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => Action::Escape,
            KeyCode::Enter => Action::Submit,
            KeyCode::Backspace => Action::Backspace,
            KeyCode::Char(c) => Action::Char(c),
            _ => Action::Nothing,
        }
    }

    /// Insert mode.
    fn feed_insert(key: KeyEvent) -> Action {
        // `Ctrl-Enter` runs without leaving insert mode: it is the gesture
        // whoever comes from Postman or a graphical client expects.
        if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Action::Run;
        }

        match key.code {
            KeyCode::Esc => Action::Escape,
            KeyCode::Enter => Action::Newline,
            KeyCode::Backspace => Action::Backspace,
            KeyCode::Left => Action::Motion(Motion::Left, 1),
            KeyCode::Right => Action::Motion(Motion::Right, 1),
            KeyCode::Up => Action::Motion(Motion::Up, 1),
            KeyCode::Down => Action::Motion(Motion::Down, 1),
            KeyCode::Char(c) => Action::Char(c),
            _ => Action::Nothing,
        }
    }

    /// Normal and visual modes: where the vim grammar lives.
    fn feed_normal(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Control shortcuts take part in neither counts nor operators.
        if ctrl {
            self.pending = Pending::None;
            return match key.code {
                KeyCode::Char('w') => {
                    self.pending = Pending::Window;
                    Action::Nothing
                }
                KeyCode::Char('d') => Action::Motion(Motion::HalfPageDown, 1),
                KeyCode::Char('u') => Action::Motion(Motion::HalfPageUp, 1),
                KeyCode::Char('c') => Action::Cancel,
                KeyCode::Enter => Action::Run,
                _ => Action::Nothing,
            };
        }

        // The window operator: `<C-w>` followed by a direction.
        if self.pending == Pending::Window {
            self.pending = Pending::None;
            return match key.code {
                KeyCode::Char('h') => Action::CyclePane(false),
                KeyCode::Char('l' | 'w') => Action::CyclePane(true),
                _ => Action::Nothing,
            };
        }

        // The leader key: space followed by an action.
        if self.pending == Pending::Leader {
            self.pending = Pending::None;
            return match key.code {
                KeyCode::Char('r') => Action::Run,
                KeyCode::Char('p') => Action::Preview,
                KeyCode::Char('v') => Action::ToggleVars,
                KeyCode::Char('i') => Action::ToggleRequest,
                KeyCode::Char('e') => Action::CycleEnv,
                KeyCode::Char('w') => Action::Save,
                KeyCode::Char('c') => Action::Cancel,
                _ => Action::Nothing,
            };
        }

        // `g` followed by a second key. The count survived the first
        // keystroke on purpose: `12gg` goes to line 12, as in vim.
        if self.pending == Pending::G {
            self.pending = Pending::None;
            let explicit = !self.count.is_empty();
            let count = self.count.parse::<usize>().unwrap_or(1).max(1);
            self.count.clear();

            return match key.code {
                KeyCode::Char('g') if explicit => Action::Motion(Motion::ToLine(count), 1),
                KeyCode::Char('g') => Action::Motion(Motion::FirstLine, 1),
                _ => Action::Nothing,
            };
        }

        // The count: digits accumulate; `0` only counts when a count is
        // already going, because otherwise it is the "line start" motion.
        if let KeyCode::Char(c @ '0'..='9') = key.code
            && (c != '0' || !self.count.is_empty())
        {
            self.count.push(c);
            return Action::Nothing;
        }

        // Operators are registered **before** the count is consumed, so it is
        // still alive when the second key arrives.
        match key.code {
            KeyCode::Char('g') => {
                self.pending = Pending::G;
                return Action::Nothing;
            }
            KeyCode::Char(' ') => {
                self.pending = Pending::Leader;
                self.count.clear();
                return Action::Nothing;
            }
            _ => {}
        }

        let explicit = !self.count.is_empty();
        let count = self.count.parse::<usize>().unwrap_or(1).max(1);
        self.count.clear();

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => Action::Motion(Motion::Down, count),
            KeyCode::Char('k') | KeyCode::Up => Action::Motion(Motion::Up, count),
            KeyCode::Char('h') | KeyCode::Left => Action::Motion(Motion::Left, count),
            KeyCode::Char('l') | KeyCode::Right => Action::Motion(Motion::Right, count),
            KeyCode::Char('w') => Action::Motion(Motion::WordForward, count),
            KeyCode::Char('b') => Action::Motion(Motion::WordBack, count),
            KeyCode::Char('0') => Action::Motion(Motion::LineStart, 1),
            KeyCode::Char('$') => Action::Motion(Motion::LineEnd, 1),

            // `G` with a count jumps to that line; without one, to the end.
            KeyCode::Char('G') => {
                if explicit {
                    Action::Motion(Motion::ToLine(count), 1)
                } else {
                    Action::Motion(Motion::LastLine, 1)
                }
            }

            KeyCode::Tab => Action::CyclePane(true),
            KeyCode::BackTab => Action::CyclePane(false),
            KeyCode::Enter => Action::Confirm,

            KeyCode::Char('i') => Action::SetMode(Mode::Insert),
            KeyCode::Char('a') => Action::InsertAfter,
            KeyCode::Char('o') => Action::OpenLineBelow,
            KeyCode::Char('v') => Action::SetMode(Mode::Visual),
            KeyCode::Esc => Action::Escape,

            KeyCode::Char('/') => Action::SetMode(Mode::Search),
            KeyCode::Char(':') => Action::SetMode(Mode::Command),
            KeyCode::Char('n') => Action::SearchAgain(true),
            KeyCode::Char('N') => Action::SearchAgain(false),

            KeyCode::Char('?') => Action::Help,
            KeyCode::Char('q') => Action::Quit,
            _ => Action::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Types a sequence and returns the last action.
    fn press(keys: &[KeyEvent]) -> Action {
        let mut keymap = Keymap::new();
        let mut last = Action::Nothing;
        for k in keys {
            last = keymap.feed(Mode::Normal, *k);
        }
        last
    }

    #[test]
    fn hjkl_moves_one_position() {
        assert_eq!(press(&[key('j')]), Action::Motion(Motion::Down, 1));
        assert_eq!(press(&[key('k')]), Action::Motion(Motion::Up, 1));
        assert_eq!(press(&[key('h')]), Action::Motion(Motion::Left, 1));
        assert_eq!(press(&[key('l')]), Action::Motion(Motion::Right, 1));
    }

    #[test]
    fn the_arrows_are_equivalent_to_hjkl() {
        assert_eq!(
            press(&[code(KeyCode::Down)]),
            Action::Motion(Motion::Down, 1)
        );
        assert_eq!(
            press(&[code(KeyCode::Left)]),
            Action::Motion(Motion::Left, 1)
        );
    }

    #[test]
    fn the_count_multiplies_the_motion() {
        assert_eq!(
            press(&[key('3'), key('j')]),
            Action::Motion(Motion::Down, 3)
        );
    }

    #[test]
    fn the_count_accepts_several_digits() {
        assert_eq!(
            press(&[key('1'), key('2'), key('k')]),
            Action::Motion(Motion::Up, 12)
        );
    }

    #[test]
    fn the_count_is_consumed_when_used() {
        let mut keymap = Keymap::new();
        let _ = keymap.feed(Mode::Normal, key('3'));
        let _ = keymap.feed(Mode::Normal, key('j'));

        assert_eq!(
            keymap.feed(Mode::Normal, key('j')),
            Action::Motion(Motion::Down, 1)
        );
    }

    #[test]
    fn gg_goes_to_the_first_line() {
        assert_eq!(
            press(&[key('g'), key('g')]),
            Action::Motion(Motion::FirstLine, 1)
        );
    }

    #[test]
    fn gg_with_a_count_jumps_to_that_line() {
        // The count has to survive the first `g`, as in vim.
        assert_eq!(
            press(&[key('1'), key('2'), key('g'), key('g')]),
            Action::Motion(Motion::ToLine(12), 1)
        );
    }

    #[test]
    fn a_lone_g_does_nothing_yet() {
        assert_eq!(press(&[key('g')]), Action::Nothing);
    }

    #[test]
    fn g_followed_by_another_key_is_discarded() {
        assert_eq!(press(&[key('g'), key('x')]), Action::Nothing);
    }

    #[test]
    fn capital_g_without_a_count_goes_to_the_end() {
        assert_eq!(press(&[key('G')]), Action::Motion(Motion::LastLine, 1));
    }

    #[test]
    fn capital_g_with_a_count_jumps_to_that_line() {
        assert_eq!(
            press(&[key('4'), key('2'), key('G')]),
            Action::Motion(Motion::ToLine(42), 1)
        );
    }

    #[test]
    fn zero_without_a_count_is_the_line_start() {
        assert_eq!(press(&[key('0')]), Action::Motion(Motion::LineStart, 1));
    }

    #[test]
    fn zero_inside_a_count_is_a_digit() {
        assert_eq!(
            press(&[key('1'), key('0'), key('j')]),
            Action::Motion(Motion::Down, 10)
        );
    }

    #[test]
    fn dollar_is_the_line_end() {
        assert_eq!(press(&[key('$')]), Action::Motion(Motion::LineEnd, 1));
    }

    #[test]
    fn w_and_b_move_by_words() {
        assert_eq!(press(&[key('w')]), Action::Motion(Motion::WordForward, 1));
        assert_eq!(
            press(&[key('2'), key('b')]),
            Action::Motion(Motion::WordBack, 2)
        );
    }

    #[test]
    fn control_d_and_u_are_half_a_page() {
        assert_eq!(press(&[ctrl('d')]), Action::Motion(Motion::HalfPageDown, 1));
        assert_eq!(press(&[ctrl('u')]), Action::Motion(Motion::HalfPageUp, 1));
    }

    #[test]
    fn control_w_plus_a_direction_changes_pane() {
        assert_eq!(press(&[ctrl('w'), key('l')]), Action::CyclePane(true));
        assert_eq!(press(&[ctrl('w'), key('h')]), Action::CyclePane(false));
    }

    #[test]
    fn control_w_alone_does_nothing_yet() {
        assert_eq!(press(&[ctrl('w')]), Action::Nothing);
    }

    #[test]
    fn tab_cycles_through_the_panes() {
        assert_eq!(press(&[code(KeyCode::Tab)]), Action::CyclePane(true));
        assert_eq!(press(&[code(KeyCode::BackTab)]), Action::CyclePane(false));
    }

    #[test]
    fn the_leader_is_the_space_key() {
        assert_eq!(press(&[key(' '), key('r')]), Action::Run);
        assert_eq!(press(&[key(' '), key('p')]), Action::Preview);
        assert_eq!(press(&[key(' '), key('v')]), Action::ToggleVars);
        assert_eq!(press(&[key(' '), key('i')]), Action::ToggleRequest);
        assert_eq!(press(&[key(' '), key('e')]), Action::CycleEnv);
        assert_eq!(press(&[key(' '), key('w')]), Action::Save);
    }

    #[test]
    fn space_alone_does_nothing_yet() {
        assert_eq!(press(&[key(' ')]), Action::Nothing);
    }

    #[test]
    fn an_unassigned_leader_key_is_discarded() {
        assert_eq!(press(&[key(' '), key('z')]), Action::Nothing);
    }

    #[test]
    fn enters_the_editing_modes() {
        assert_eq!(press(&[key('i')]), Action::SetMode(Mode::Insert));
        assert_eq!(press(&[key('a')]), Action::InsertAfter);
        assert_eq!(press(&[key('o')]), Action::OpenLineBelow);
        assert_eq!(press(&[key('v')]), Action::SetMode(Mode::Visual));
    }

    #[test]
    fn opens_search_and_the_command_line() {
        assert_eq!(press(&[key('/')]), Action::SetMode(Mode::Search));
        assert_eq!(press(&[key(':')]), Action::SetMode(Mode::Command));
    }

    #[test]
    fn n_repeats_the_search_both_ways() {
        assert_eq!(press(&[key('n')]), Action::SearchAgain(true));
        assert_eq!(press(&[key('N')]), Action::SearchAgain(false));
    }

    #[test]
    fn in_insert_mode_letters_are_text() {
        let mut keymap = Keymap::new();
        assert_eq!(keymap.feed(Mode::Insert, key('j')), Action::Char('j'));
        assert_eq!(keymap.feed(Mode::Insert, key('3')), Action::Char('3'));
    }

    #[test]
    fn control_enter_runs_the_request_in_normal_mode() {
        assert_eq!(
            press(&[KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)]),
            Action::Run
        );
    }

    #[test]
    fn control_enter_runs_the_request_without_leaving_insert_mode() {
        let mut keymap = Keymap::new();
        assert_eq!(
            keymap.feed(
                Mode::Insert,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)
            ),
            Action::Run
        );
    }

    #[test]
    fn a_bare_enter_still_splits_the_line() {
        let mut keymap = Keymap::new();
        assert_eq!(
            keymap.feed(Mode::Insert, code(KeyCode::Enter)),
            Action::Newline
        );
    }

    #[test]
    fn escape_leaves_insert_mode() {
        let mut keymap = Keymap::new();
        assert_eq!(
            keymap.feed(Mode::Insert, code(KeyCode::Esc)),
            Action::Escape
        );
    }

    #[test]
    fn on_the_command_line_letters_are_text() {
        let mut keymap = Keymap::new();
        assert_eq!(keymap.feed(Mode::Command, key('r')), Action::Char('r'));
        assert_eq!(
            keymap.feed(Mode::Command, code(KeyCode::Enter)),
            Action::Submit
        );
        assert_eq!(
            keymap.feed(Mode::Command, code(KeyCode::Backspace)),
            Action::Backspace
        );
    }

    #[test]
    fn the_hint_shows_what_is_half_typed() {
        let mut keymap = Keymap::new();
        let _ = keymap.feed(Mode::Normal, key('1'));
        let _ = keymap.feed(Mode::Normal, key('2'));
        assert_eq!(keymap.hint(), "12");

        let _ = keymap.feed(Mode::Normal, key('g'));
        assert_eq!(keymap.hint(), "12g");

        keymap.reset();
        assert_eq!(keymap.hint(), "");
    }

    #[test]
    fn the_hint_tells_the_window_operator_from_the_leader() {
        let mut keymap = Keymap::new();
        let _ = keymap.feed(Mode::Normal, ctrl('w'));
        assert_eq!(keymap.hint(), "^W");

        keymap.reset();
        let _ = keymap.feed(Mode::Normal, key(' '));
        assert_eq!(keymap.hint(), "␣");
    }

    #[test]
    fn an_outsized_count_does_not_overflow() {
        // `parse` fails and falls back to the sensible minimum instead of panicking.
        let digits: Vec<KeyEvent> = "99999999999999999999999".chars().map(key).collect();
        let mut keys = digits;
        keys.push(key('j'));

        assert_eq!(press(&keys), Action::Motion(Motion::Down, 1));
    }
}
