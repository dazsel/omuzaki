//! The chat-emote cell allocator (D13): which atlas cell holds which chat emote.
//!
//! The atlas is a grid of cells in one texture. Cell 0 is the placeholder (a heart drawn by code) and is never handed
//! out. This module only decides where an emote goes: it does no fetching, uploading or rendering, and it never
//! allocates after [`EmoteAtlas::new`]. Keys are opaque `u64`s (a hash of the emote's URL, D13).

/// The most keys one [`EmoteAtlas::cells_for`] call takes: a burst carries at most 8 emotes (D13).
pub const MAX_PICK: usize = 8;

/// How many keys may be loading at once (D13). A request beyond this is [`Request::Busy`].
const MAX_LOADING: usize = 8;

/// The placeholder cell, which every key that is not ready draws.
const PLACEHOLDER: u16 = 0;

/// The most cells an atlas can have. Cell indices are `u16`, so a larger grid is clipped to this many cells.
const MAX_CELLS: u32 = 65_535;

/// What [`EmoteAtlas::lookup`] found for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// The emote is in this cell and can be drawn.
    Ready(u16),
    /// The emote is being fetched; draw the placeholder until it arrives.
    Loading,
    /// The atlas has no record of the key; draw the placeholder.
    Missing,
}

/// What [`EmoteAtlas::request`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// The key is now loading. The caller fetches it, then calls [`EmoteAtlas::insert`].
    Started,
    /// The key is already loading or ready, so there is nothing to fetch.
    AlreadyKnown,
    /// [`MAX_LOADING`] keys are already loading. Try again once one finishes.
    Busy,
}

/// One cell's bookkeeping.
#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    /// The key whose emote is in the cell, or `None` when the cell is free.
    key: Option<u64>,
    /// The frame the cell was last used on. The least recently used cell has the smallest stamp.
    stamp: u64,
    /// How many live batches hold the cell. A pinned cell is never handed out or evicted.
    pins: u32,
}

/// The cell allocator for chat emotes: a fixed grid of cells, least-recently-used eviction, pins and a request budget.
///
/// Cell 0 is the placeholder and is never handed out. Ties in the LRU stamp go to the lowest cell index, so the same
/// sequence of calls always gives the same cells.
#[derive(Debug)]
pub struct EmoteAtlas {
    /// The number of cells, placeholder included. At least 1.
    count: u16,
    /// One slot per cell, indexed by cell number.
    slots: Vec<Slot>,
    /// The keys being fetched, `None` where a request slot is free.
    loading: [Option<u64>; MAX_LOADING],
}

impl EmoteAtlas {
    /// An atlas of `columns * rows` cells. Cell 0 is the placeholder, so `columns * rows` must leave room for it; a
    /// grid of no cells keeps just the placeholder, and a grid larger than [`u16::MAX`] cells is clipped to it.
    pub fn new(columns: u16, rows: u16) -> Self {
        let cells = (u32::from(columns) * u32::from(rows)).clamp(1, MAX_CELLS);
        // The clamp keeps `cells` within `u16`, so the conversion cannot fail.
        let count = u16::try_from(cells).unwrap_or(u16::MAX);
        Self { count, slots: vec![Slot::default(); usize::from(count)], loading: [None; MAX_LOADING] }
    }

    /// The chat atlas: 8 x 8 cells, so 63 emotes fit beside the placeholder (D13).
    pub fn chat() -> Self {
        Self::new(8, 8)
    }

    /// Where `key` is: [`Cell::Ready`] with its cell (touching its LRU stamp to `frame`), [`Cell::Loading`] while it
    /// is being fetched, else [`Cell::Missing`].
    pub fn lookup(&mut self, key: u64, frame: u64) -> Cell {
        if let Some(cell) = self.find(key) {
            self.touch(cell, frame);
            return Cell::Ready(cell);
        }
        if self.is_loading(key) { Cell::Loading } else { Cell::Missing }
    }

    /// Asks for `key` to be fetched. Returns [`Request::Started`] and marks the key loading, or
    /// [`Request::AlreadyKnown`] if it is already loading or ready, or [`Request::Busy`] when every request slot is in
    /// use.
    pub fn request(&mut self, key: u64) -> Request {
        if self.find(key).is_some() || self.is_loading(key) {
            return Request::AlreadyKnown;
        }
        let Some(slot) = self.loading.iter_mut().find(|slot| slot.is_none()) else {
            return Request::Busy;
        };
        *slot = Some(key);
        Request::Started
    }

    /// Stores the fetched emote for `key` and returns the cell to upload it into. The key stops loading. A free cell
    /// is used first, else the least recently used unpinned ready cell is evicted. Returns `None` when every cell is
    /// pinned: the key then goes back to [`Cell::Missing`].
    ///
    /// A key that is already ready keeps its cell; the call only touches its stamp to `frame`.
    pub fn insert(&mut self, key: u64, frame: u64) -> Option<u16> {
        self.finish_loading(key);
        if let Some(cell) = self.find(key) {
            self.touch(cell, frame);
            return Some(cell);
        }
        let cell = self.free_or_victim()?;
        self.slots[usize::from(cell)] = Slot { key: Some(key), stamp: frame, pins: 0 };
        Some(cell)
    }

    /// Pins `cell` for one live batch, so it is not evicted. Each call adds one pin; a cell pinned twice needs two
    /// [`unpin`](Self::unpin)s. A cell number outside the grid is ignored.
    pub fn pin(&mut self, cell: u16) {
        if let Some(slot) = self.slots.get_mut(usize::from(cell)) {
            slot.pins = slot.pins.saturating_add(1);
        }
    }

    /// Releases one pin on `cell`. Unpinning a cell that has no pins is a bug: it trips a debug assertion, and in a
    /// release build it does nothing. A cell number outside the grid is ignored.
    pub fn unpin(&mut self, cell: u16) {
        let Some(slot) = self.slots.get_mut(usize::from(cell)) else {
            return;
        };
        debug_assert!(slot.pins > 0, "unpin of cell {cell}, which is not pinned");
        slot.pins = slot.pins.saturating_sub(1);
    }

    /// Resolves a burst's keys to cells for a frame. Writes one cell per key into `out` and returns how many it wrote,
    /// which is `keys.len()` capped at [`MAX_PICK`]. A key that is not ready gets the placeholder, 0. A ready key's
    /// LRU stamp is touched to `frame`, as [`lookup`](Self::lookup) does.
    pub fn cells_for(&mut self, keys: &[u64], frame: u64, out: &mut [u16; MAX_PICK]) -> usize {
        let written = keys.len().min(MAX_PICK);
        for (dst, &key) in out.iter_mut().zip(keys) {
            *dst = match self.find(key) {
                Some(cell) => {
                    self.touch(cell, frame);
                    cell
                }
                None => PLACEHOLDER,
            };
        }
        written
    }

    /// The ready cell holding `key`, if any.
    fn find(&self, key: u64) -> Option<u16> {
        (1..self.count).find(|&cell| self.slots[usize::from(cell)].key == Some(key))
    }

    /// Records a use of `cell` on `frame`.
    fn touch(&mut self, cell: u16, frame: u64) {
        self.slots[usize::from(cell)].stamp = frame;
    }

    /// Whether `key` has a request slot.
    fn is_loading(&self, key: u64) -> bool {
        self.loading.contains(&Some(key))
    }

    /// Frees the request slot of `key`, if it has one.
    fn finish_loading(&mut self, key: u64) {
        if let Some(slot) = self.loading.iter_mut().find(|slot| **slot == Some(key)) {
            *slot = None;
        }
    }

    /// The cell a new key goes into. Pinned cells are never chosen. The lowest free cell wins. Failing that, the
    /// unpinned ready cell with the smallest stamp; a tie goes to the lowest index, since the scan keeps the first.
    fn free_or_victim(&self) -> Option<u16> {
        let mut victim: Option<(u64, u16)> = None;
        for cell in 1..self.count {
            let slot = &self.slots[usize::from(cell)];
            if slot.pins > 0 {
                continue;
            }
            if slot.key.is_none() {
                return Some(cell);
            }
            if victim.is_none_or(|(stamp, _)| slot.stamp < stamp) {
                victim = Some((slot.stamp, cell));
            }
        }
        victim.map(|(_, cell)| cell)
    }
}
