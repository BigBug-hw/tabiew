//! Hex-dump formatting and byte-cursor arithmetic.
//!
//! Pure functions, no UI dependencies — the [`crate::tui::hex_viewer`] view
//! calls these to render rows and compute cursor movement.

/// Number of bytes per hex row.
pub const BYTES_PER_ROW: usize = 16;

/// A single rendered hex row.
#[derive(Debug, Clone)]
pub struct HexRow {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

/// Split `bytes` (a window starting at absolute `base_offset`) into rows of
/// [`BYTES_PER_ROW`] bytes each.
pub fn rows(bytes: &[u8], base_offset: u64) -> Vec<HexRow> {
    bytes
        .chunks(BYTES_PER_ROW)
        .enumerate()
        .map(|(i, chunk)| HexRow {
            offset: base_offset + (i * BYTES_PER_ROW) as u64,
            bytes: chunk.to_vec(),
        })
        .collect()
}

/// Format an offset as `0x%08x`.
pub fn format_offset(o: u64) -> String {
    format!("0x{:08x}", o)
}

/// Format a byte as two lowercase hex digits.
pub fn format_byte(b: u8) -> String {
    format!("{:02x}", b)
}

/// Format a byte for the ASCII column (`.` for non-printable).
pub fn format_ascii(b: u8) -> char {
    if (0x20..0x7f).contains(&b) {
        b as char
    } else {
        '.'
    }
}

/// Cursor movement over a byte buffer of `len` bytes.
#[derive(Debug, Clone, Copy)]
pub struct Cursor {
    pub pos: u64,
    pub len: u64,
}

impl Cursor {
    pub fn new(len: u64) -> Self {
        Self { pos: 0, len }
    }

    pub fn clamp(&mut self) {
        if self.len == 0 {
            self.pos = 0;
        } else if self.pos >= self.len {
            self.pos = self.len - 1;
        }
    }

    pub fn left(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.pos + 1 < self.len {
            self.pos += 1;
        }
    }

    pub fn up(&mut self) {
        self.pos = self.pos.saturating_sub(BYTES_PER_ROW as u64);
    }

    pub fn down(&mut self) {
        let next = self.pos + BYTES_PER_ROW as u64;
        if next < self.len {
            self.pos = next;
        } else {
            // Last row: keep within bounds.
            self.pos = self.len.saturating_sub(1);
        }
    }

    pub fn row_start(&mut self) {
        let row_off = self.pos % BYTES_PER_ROW as u64;
        self.pos = self.pos.saturating_sub(row_off);
    }

    pub fn row_end(&mut self) {
        let row_off = self.pos % BYTES_PER_ROW as u64;
        let candidate = self.pos + (BYTES_PER_ROW as u64 - 1 - row_off);
        self.pos = candidate.min(self.len.saturating_sub(1));
    }

    pub fn page_up(&mut self, page_bytes: u64) {
        self.pos = self.pos.saturating_sub(page_bytes);
    }

    pub fn page_down(&mut self, page_bytes: u64) {
        self.pos = (self.pos + page_bytes).min(self.len.saturating_sub(1));
    }

    pub fn home(&mut self) {
        self.pos = 0;
    }

    pub fn end(&mut self) {
        self.pos = self.len.saturating_sub(1);
    }

    pub fn goto(&mut self, pos: u64) {
        self.pos = pos.min(self.len.saturating_sub(1));
    }

    /// Absolute offset of the first byte of the row containing `pos`.
    pub fn row_of(pos: u64) -> u64 {
        (pos / BYTES_PER_ROW as u64) * BYTES_PER_ROW as u64
    }

    /// Index within its row of `pos`.
    pub fn col_of(pos: u64) -> usize {
        (pos % BYTES_PER_ROW as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_split_correctly() {
        let bytes: Vec<u8> = (0..34).collect();
        let rs = rows(&bytes, 100);
        assert_eq!(rs.len(), 3);
        assert_eq!(rs[0].offset, 100);
        assert_eq!(rs[0].bytes.len(), 16);
        assert_eq!(rs[1].offset, 116);
        assert_eq!(rs[2].offset, 132);
        assert_eq!(rs[2].bytes.len(), 2);
    }

    #[test]
    fn format_helpers() {
        assert_eq!(format_offset(0x4d2), "0x000004d2");
        assert_eq!(format_byte(0xab), "ab");
        assert_eq!(format_ascii(0x41), 'A');
        assert_eq!(format_ascii(0x00), '.');
        assert_eq!(format_ascii(0x7f), '.');
    }

    #[test]
    fn cursor_basic_moves() {
        let mut c = Cursor::new(100);
        c.right();
        assert_eq!(c.pos, 1);
        c.down();
        assert_eq!(c.pos, 17);
        c.up();
        assert_eq!(c.pos, 1);
        c.row_end();
        assert_eq!(c.pos, 15);
        c.row_start();
        assert_eq!(c.pos, 0);
        c.end();
        assert_eq!(c.pos, 99);
        c.home();
        assert_eq!(c.pos, 0);
    }

    #[test]
    fn cursor_page_and_clamp() {
        let mut c = Cursor::new(50);
        c.page_down(32);
        assert_eq!(c.pos, 32);
        c.page_down(32);
        assert_eq!(c.pos, 49); // clamped to last byte
        c.page_up(100);
        assert_eq!(c.pos, 0);
    }

    #[test]
    fn cursor_row_helpers() {
        assert_eq!(Cursor::row_of(20), 16);
        assert_eq!(Cursor::col_of(20), 4);
    }

    #[test]
    fn cursor_empty() {
        let mut c = Cursor::new(0);
        c.right();
        assert_eq!(c.pos, 0);
        c.end();
        assert_eq!(c.pos, 0);
    }
}
