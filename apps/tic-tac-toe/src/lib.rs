//! Tic-tac-toe — Akiva's second app.
//!
//! Exercises the same widget toolkit the calculator uses (stacks, Grid,
//! Buttons, SavedState) on a different workload: a 3×3 board with turn
//! tracking, win detection, and draw detection. Proves the platform
//! isn't hard-coded around a single app shape.
//!
//! Like the calculator, the rules engine (`Game`) is pure Rust with no
//! SDK types on its boundaries — trivially unit-testable without a host.
//! The Activity glue builds a fresh `Bindings` per frame so the cell
//! labels and the status line reflect the current state.

use akiva_sdk::prelude::*;
use akiva_sdk::akml::{self, Bindings};
use akiva_sdk::res;

/// The UI in AKML, embedded into the wasm so there's no layout loader
/// or I/O path to think about. Same pattern the calculator uses.
const LAYOUT: &str = include_str!("../res/layout/main.akml");

// ── Game rules ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell { Empty, X, O }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Player { X, O }

impl Player {
    fn other(self) -> Player {
        match self { Player::X => Player::O, Player::O => Player::X }
    }
    fn glyph(self) -> &'static str {
        match self { Player::X => "X", Player::O => "O" }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status { Playing, Won(Player), Draw }

/// Cells laid out row-major: 0..=2 top row, 3..=5 middle, 6..=8 bottom.
/// `turn` is *who plays next*. Once a line completes, `status` latches
/// and further `play()` calls are ignored until `reset()`.
#[derive(Debug)]
pub struct Game {
    cells: [Cell; 9],
    turn: Player,
    status: Status,
}

/// All eight three-in-a-row lines on a 3×3 board: three rows, three
/// columns, two diagonals. Referenced by `derive_status`.
const LINES: [[usize; 3]; 8] = [
    [0, 1, 2], [3, 4, 5], [6, 7, 8],
    [0, 3, 6], [1, 4, 7], [2, 5, 8],
    [0, 4, 8], [2, 4, 6],
];

impl Default for Game {
    fn default() -> Self { Self::new() }
}

impl Game {
    pub fn new() -> Self {
        Self { cells: [Cell::Empty; 9], turn: Player::X, status: Status::Playing }
    }

    pub fn cell(&self, i: usize) -> Cell { self.cells[i] }
    pub fn turn(&self) -> Player { self.turn }
    pub fn status(&self) -> Status { self.status }

    pub fn reset(&mut self) { *self = Self::new(); }

    /// Play cell `i` for whoever's turn it is. Silent no-op on:
    ///   - game already over (Won/Draw),
    ///   - cell already taken,
    ///   - out-of-range index.
    /// Keeps callers honest without forcing them to double-check state.
    pub fn play(&mut self, i: usize) {
        if self.status != Status::Playing { return; }
        if i >= 9 || self.cells[i] != Cell::Empty { return; }
        self.cells[i] = match self.turn { Player::X => Cell::X, Player::O => Cell::O };
        self.status = derive_status(&self.cells);
        if self.status == Status::Playing {
            self.turn = self.turn.other();
        }
    }

    /// One-glyph label for a cell, used as a binding in the AKML grid.
    /// Empty cells render as a non-breaking space so the Button still
    /// lays out at the same size — an empty string would collapse.
    pub fn cell_glyph(&self, i: usize) -> &'static str {
        match self.cells[i] {
            Cell::Empty => " ",
            Cell::X     => "X",
            Cell::O     => "O",
        }
    }

    pub fn status_text(&self) -> String {
        match self.status {
            Status::Playing => format!("{}'s turn", self.turn.glyph()),
            Status::Won(p)  => format!("{} wins!", p.glyph()),
            Status::Draw    => "Draw".into(),
        }
    }
}

/// Classify the current board. Pure function of `cells` — turn doesn't
/// matter, since a line of X is a win whether or not it was X's turn to
/// move. Called after every `play` and on restore.
fn derive_status(cells: &[Cell; 9]) -> Status {
    for [a, b, c] in LINES {
        if cells[a] != Cell::Empty && cells[a] == cells[b] && cells[b] == cells[c] {
            return match cells[a] {
                Cell::X => Status::Won(Player::X),
                Cell::O => Status::Won(Player::O),
                Cell::Empty => unreachable!("guarded above"),
            };
        }
    }
    if cells.iter().all(|c| *c != Cell::Empty) { Status::Draw } else { Status::Playing }
}

// ── Activity glue ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct TicTacToeMain {
    ui: UiState,
    game: Game,
}

#[async_trait(?Send)]
impl AsyncActivity for TicTacToeMain {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!("TicTacToeMain created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    /// Persist just the 9 cells as a fixed-length string. Turn and
    /// status are derivable from the board (see `derive_status` and the
    /// X/O count parity in restore), so saving them would only create a
    /// second source of truth that could drift.
    fn on_save_state(&self, state: &mut SavedState) {
        let mut s = String::with_capacity(9);
        for c in self.game.cells {
            s.push(match c { Cell::Empty => '.', Cell::X => 'X', Cell::O => 'O' });
        }
        state.put_string("b", &s);
    }

    fn on_restore_state(&mut self, state: &SavedState) {
        let Some(s) = state.get_string("b") else { return; };
        if s.len() != 9 { return; }
        let mut cells = [Cell::Empty; 9];
        for (i, ch) in s.chars().enumerate() {
            cells[i] = match ch { 'X' => Cell::X, 'O' => Cell::O, _ => Cell::Empty };
        }
        self.game.cells = cells;
        self.game.status = derive_status(&cells);
        // X always starts, so when X and O counts are equal it's X's
        // turn next; otherwise it's O's. This is a total function of
        // the board and agrees with play-order by construction.
        let xs = cells.iter().filter(|c| **c == Cell::X).count();
        let os = cells.iter().filter(|c| **c == Cell::O).count();
        self.game.turn = if xs == os { Player::X } else { Player::O };
        log_info!("TicTacToe restored: board={:?} turn={:?} status={:?}",
            s, self.game.turn, self.game.status);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(res::color("bg_window"));

        // Rebuild bindings every frame. The status line and the nine
        // cell labels all derive from game state; the calculator uses
        // the same shape for its `display` / `hint` bindings.
        const CELL_KEYS: [&str; 9] = ["c0","c1","c2","c3","c4","c5","c6","c7","c8"];
        let mut bindings = Bindings::new().text("status", self.game.status_text());
        for (i, key) in CELL_KEYS.iter().enumerate() {
            bindings = bindings.text(*key, self.game.cell_glyph(i));
        }

        let mut ui = Ui::new(&mut self.ui, ctx, c);
        let root = akml::inflate_str_with(&mut ui, LAYOUT, &bindings)
            .expect("tic-tac-toe layout is statically valid — inflate failure is a build bug");
        ui.present(root);

        // ── Dispatch clicks ──
        // Cell actions are `cell_0` through `cell_8`; the reset button
        // uses `reset`. Action names are the stable contract with
        // AKML — ids can be renamed for a11y without touching any of
        // this code.
        const CELL_ACTIONS: [&str; 9] = [
            "cell_0","cell_1","cell_2","cell_3","cell_4",
            "cell_5","cell_6","cell_7","cell_8",
        ];
        for (i, action) in CELL_ACTIONS.iter().enumerate() {
            if ui.was_triggered(action) {
                self.game.play(i);
                log_debug!("play {} → turn={:?} status={:?}",
                    i, self.game.turn, self.game.status);
            }
        }
        if ui.was_triggered("reset") {
            self.game.reset();
            log_debug!("reset");
        }
    }
}

akiva_sdk::akiva_async_app!(TicTacToeMain);

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_game_is_empty_and_x_to_play() {
        let g = Game::new();
        assert_eq!(g.status(), Status::Playing);
        assert_eq!(g.turn(), Player::X);
        for i in 0..9 { assert_eq!(g.cell(i), Cell::Empty); }
    }

    #[test]
    fn play_places_glyph_and_toggles_turn() {
        let mut g = Game::new();
        g.play(0);
        assert_eq!(g.cell(0), Cell::X);
        assert_eq!(g.turn(), Player::O);
        g.play(4);
        assert_eq!(g.cell(4), Cell::O);
        assert_eq!(g.turn(), Player::X);
    }

    #[test]
    fn play_on_taken_cell_is_noop() {
        let mut g = Game::new();
        g.play(0);                       // X takes 0
        let turn_before = g.turn();      // O up next
        g.play(0);                       // still taken — ignored
        assert_eq!(g.cell(0), Cell::X);
        assert_eq!(g.turn(), turn_before);
    }

    #[test]
    fn play_out_of_range_is_noop() {
        let mut g = Game::new();
        g.play(99);
        assert_eq!(g.turn(), Player::X);
        assert!(g.cells.iter().all(|c| *c == Cell::Empty));
    }

    #[test]
    fn row_win_detected() {
        // X: 0,1,2 (top row); O: 3,4 (spread)
        let mut g = Game::new();
        g.play(0); g.play(3);
        g.play(1); g.play(4);
        g.play(2);
        assert_eq!(g.status(), Status::Won(Player::X));
    }

    #[test]
    fn column_win_detected() {
        // O: 1,4,7 (middle column); X: 0,2 (top row ends)
        let mut g = Game::new();
        g.play(0); g.play(1);
        g.play(2); g.play(4);
        g.play(8); g.play(7);
        assert_eq!(g.status(), Status::Won(Player::O));
    }

    #[test]
    fn diagonal_win_detected() {
        // X: 0,4,8 (main diagonal)
        let mut g = Game::new();
        g.play(0); g.play(1);
        g.play(4); g.play(2);
        g.play(8);
        assert_eq!(g.status(), Status::Won(Player::X));
    }

    #[test]
    fn anti_diagonal_win_detected() {
        // X: 2,4,6 (anti-diagonal)
        let mut g = Game::new();
        g.play(2); g.play(0);
        g.play(4); g.play(1);
        g.play(6);
        assert_eq!(g.status(), Status::Won(Player::X));
    }

    #[test]
    fn draw_detected_when_full_and_no_win() {
        // A classic cat's game:
        //   X O X
        //   X O O
        //   O X X
        let mut g = Game::new();
        // X,O,X,X,O,O,O,X,X — encoded as the play sequence below.
        for (i, &idx) in [0, 1, 2, 4, 3, 5, 7, 6, 8].iter().enumerate() {
            // Alternating by position in the sequence — even=X, odd=O —
            // matches `turn` because play() toggles it automatically.
            let _ = i;
            g.play(idx);
        }
        assert_eq!(g.status(), Status::Draw);
        // Every cell filled.
        assert!(g.cells.iter().all(|c| *c != Cell::Empty));
    }

    #[test]
    fn play_after_win_is_noop() {
        let mut g = Game::new();
        g.play(0); g.play(3);
        g.play(1); g.play(4);
        g.play(2); // X wins top row
        assert_eq!(g.status(), Status::Won(Player::X));
        g.play(8); // should be ignored
        assert_eq!(g.cell(8), Cell::Empty);
    }

    #[test]
    fn reset_clears_board_and_status() {
        let mut g = Game::new();
        g.play(0); g.play(4); g.play(1);
        g.reset();
        assert_eq!(g.status(), Status::Playing);
        assert_eq!(g.turn(), Player::X);
        for i in 0..9 { assert_eq!(g.cell(i), Cell::Empty); }
    }

    #[test]
    fn cell_glyphs_match_state() {
        let mut g = Game::new();
        assert_eq!(g.cell_glyph(0), " ");
        g.play(0);
        assert_eq!(g.cell_glyph(0), "X");
        g.play(1);
        assert_eq!(g.cell_glyph(1), "O");
    }

    #[test]
    fn status_text_reports_turn_and_result() {
        let mut g = Game::new();
        assert_eq!(g.status_text(), "X's turn");
        g.play(0);
        assert_eq!(g.status_text(), "O's turn");
        // Force a win for X.
        g.play(3);
        g.play(1);
        g.play(4);
        g.play(2);
        assert_eq!(g.status_text(), "X wins!");
    }

    #[test]
    fn derive_status_is_pure_on_the_board() {
        // The restore path relies on `derive_status` not looking at
        // turn. Plant a won board by hand and check.
        let mut cells = [Cell::Empty; 9];
        cells[0] = Cell::O; cells[4] = Cell::O; cells[8] = Cell::O;
        assert_eq!(derive_status(&cells), Status::Won(Player::O));
    }
}
