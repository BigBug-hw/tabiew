//! Full-screen hex viewer with optional Parquet structure awareness.
//!
//! Left pane: hex dump with a movable byte cursor (windowed reads via
//! [`ByteSource`], so large files are never fully loaded). Right pane: a
//! "cursor location" panel (which region/wire field the cursor is in) and a
//! scrollable structure tree (for Parquet files).

use std::{collections::VecDeque, path::PathBuf, sync::Arc};

use crate::binary::parquet_decode::{PageDetails, decode_page};

const MAX_STRUCTURE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DETAIL_CACHE: usize = 128 * 1024 * 1024;

struct DetailView {
    title: String,
    field_range: Option<std::ops::Range<u64>>,
    page: Option<Arc<PageDetails>>,
    text: Vec<String>,
    tab: usize,
    scroll: [usize; 3],
    horizontal: usize,
}
#[derive(Clone)]
struct NavigationPosition {
    offset: u64,
    scroll: u64,
    tree_sel: usize,
    tree_scroll: u16,
    focus: Focus,
    collapsed: std::collections::HashSet<usize>,
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use crate::{
    AppResult,
    binary::{
        hex::{BYTES_PER_ROW, Cursor, format_ascii, format_byte, format_offset, rows},
        layout::{FileLayout, UnifiedRegion, dispatch},
        source::ByteSource,
    },
    handler::message::Message,
    misc::config::theme,
    tui::{
        app_default::{AppDefault, AppTitle},
        component::{Component, FocusState},
        widgets::input::{Input, InputType},
    },
};

/// Which pane currently receives cursor movement keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Hex,
    Tree,
}

/// The hex viewer component.
pub struct HexViewer {
    name: String,
    source: ByteSource,
    layout: FileLayout,
    /// Cached format-agnostic regions (computed once from `layout`).
    regions: Vec<UnifiedRegion>,
    cursor: Cursor,
    /// First visible absolute offset.
    scroll: u64,
    /// Number of hex rows visible in the last render (set in `render`).
    visible_rows: u16,
    focus: Focus,
    /// Tree selection index into `layout.tree`.
    tree_sel: usize,
    /// Tree vertical scroll.
    tree_scroll: u16,
    /// Set of collapsed tree node indices (by node index).
    collapsed: std::collections::HashSet<usize>,
    /// Ctrl-g offset input.
    goto_input: Option<Input>,
    /// Cached I/O / decode error to render.
    error: Option<String>,
    /// Number of tree rows visible in the last render (set in `render`).
    visible_tree_rows: u16,
    detail: Option<DetailView>,
    detail_cache: VecDeque<(u64, Arc<PageDetails>, usize)>,
    history: Vec<NavigationPosition>,
}

impl HexViewer {
    /// Open a file by path. Footer decoding happens immediately; page-header
    /// scanning is bounded by the parser's internal limits.
    pub fn open_path(path: impl Into<PathBuf>) -> AppResult<Self> {
        let path: PathBuf = path.into();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let mut source = ByteSource::open(&path)?;
        let layout = if source.len() > MAX_STRUCTURE_BYTES {
            FileLayout::Unknown {
                error: Some(format!(
                    "Structure parsing skipped: file exceeds {} MiB; raw hex remains available",
                    MAX_STRUCTURE_BYTES / 1024 / 1024
                )),
            }
        } else {
            dispatch(&source.read_range(0, source.len() as usize)?)
        };
        let len = source.len();
        let regions = layout.unified_regions();
        let collapsed = default_collapsed(&layout);
        Ok(Self {
            name,
            source,
            layout,
            regions,
            cursor: Cursor::new(len),
            scroll: 0,
            visible_rows: 0,
            focus: Focus::Hex,
            tree_sel: 0,
            tree_scroll: 0,
            collapsed,
            goto_input: None,
            error: None,
            visible_tree_rows: 0,
            detail: None,
            detail_cache: VecDeque::new(),
            history: Vec::new(),
        })
    }

    /// Build a viewer from an in-memory buffer (stdin / tests).
    pub fn from_bytes(name: impl Into<String>, data: Vec<u8>) -> Self {
        let len = data.len() as u64;
        let layout = if len > MAX_STRUCTURE_BYTES {
            FileLayout::Unknown {
                error: Some(
                    "Structure parsing skipped: input exceeds 256 MiB; raw hex remains available"
                        .into(),
                ),
            }
        } else {
            dispatch(&data)
        };
        let source = ByteSource::from_bytes(data);
        let regions = layout.unified_regions();
        let collapsed = default_collapsed(&layout);
        Self {
            name: name.into(),
            source,
            layout,
            regions,
            cursor: Cursor::new(len),
            scroll: 0,
            visible_rows: 0,
            focus: Focus::Hex,
            tree_sel: 0,
            tree_scroll: 0,
            collapsed,
            goto_input: None,
            error: None,
            visible_tree_rows: 0,
            detail: None,
            detail_cache: VecDeque::new(),
            history: Vec::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Find the **innermost** region containing `offset`, if any.
    /// Regions are nested (e.g. RowGroup ⊃ ColumnChunk ⊃ PageHeader/PageData
    /// for Parquet, or Manifest ⊃ ManifestField for Lance), so we pick the one
    /// with the smallest span — that's the leaf.
    fn region_at(&self, offset: u64) -> Option<&UnifiedRegion> {
        self.regions
            .iter()
            .filter(|r| offset >= r.span.start && offset < r.span.end)
            .min_by_key(|r| r.span.end - r.span.start)
    }

    /// Move scroll so the cursor is visible.
    fn ensure_cursor_visible(&mut self) {
        let row = self.cursor.pos / BYTES_PER_ROW as u64;
        if row < self.scroll {
            self.scroll = row;
        } else if self.visible_rows > 0 {
            let last_visible = self.scroll + self.visible_rows as u64 - 1;
            if row > last_visible {
                self.scroll = row - (self.visible_rows as u64 - 1);
            }
        }
    }

    /// Jump the hex cursor to `offset` and bring it into view.
    fn jump_to(&mut self, offset: u64) {
        self.cursor.goto(offset);
        self.ensure_cursor_visible();
    }

    /// Compute the raw indices of tree nodes that are currently visible,
    /// i.e. not hidden by an collapsed ancestor. The tree is a flat list
    /// annotated with `depth`; a collapsed node at depth D hides all
    /// following nodes with depth > D until depth returns to <= D.
    fn visible_tree_indices(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.layout.tree().len());
        // Stack of (raw_index, depth) for currently-collapsed ancestors.
        let mut collapsed_stack: Vec<(usize, u8)> = Vec::new();
        for (i, n) in self.layout.tree().iter().enumerate() {
            // Pop collapsed ancestors whose depth >= current depth (they no
            // longer cover this node).
            while let Some(&(_, d)) = collapsed_stack.last() {
                if d >= n.depth {
                    collapsed_stack.pop();
                } else {
                    break;
                }
            }
            // If there's still a collapsed ancestor on the stack, this node
            // is hidden.
            if collapsed_stack.is_empty() {
                out.push(i);
            }
            // If this node is itself collapsed and has children, push it.
            if self.collapsed.contains(&i) && n.collapsible {
                collapsed_stack.push((i, n.depth));
            }
        }
        out
    }

    /// Move tree selection to the next/previous *visible* node.
    fn tree_move(&mut self, delta: i32) {
        let visible = self.visible_tree_indices();
        if visible.is_empty() {
            return;
        }
        // Find current position in the visible list.
        let cur_pos = visible
            .iter()
            .position(|&i| i == self.tree_sel)
            .unwrap_or(0);
        let len = visible.len() as i32;
        let mut next = cur_pos as i32 + delta;
        if next < 0 {
            next = 0;
        } else if next >= len {
            next = len - 1;
        }
        self.tree_sel = visible[next as usize];
        // Keep selection visible in the tree scroll window (height unknown
        // until render; we approximate using a 1-row heuristic and let render
        // refine). `tree_scroll` is in visible-row units, so compare against
        // the visible position, not the raw index.
        let h = self.visible_tree_rows.max(1);
        let new_pos = next as u16;
        if new_pos < self.tree_scroll {
            self.tree_scroll = new_pos;
        } else if new_pos >= self.tree_scroll + h {
            self.tree_scroll = new_pos.saturating_sub(h - 1);
        }
    }

    /// Ensure tree node `idx` is visible by expanding all ancestors that
    /// contain it and are currently collapsed.
    fn ensure_node_visible(&mut self, idx: usize) {
        let target = match self.layout.tree().get(idx) {
            Some(n) => n,
            None => return,
        };
        let mut depth = target.depth;
        // Ancestors are defined by tree depth, including equal-span wrappers.
        for i in (0..idx).rev() {
            let node = &self.layout.tree()[i];
            if node.depth < depth {
                self.collapsed.remove(&i);
                depth = node.depth;
            }
            if depth == 0 {
                break;
            }
        }
    }
}

impl Component for HexViewer {
    fn render(&mut self, area: Rect, buf: &mut ratatui::prelude::Buffer, _focus: FocusState) {
        buf.set_style(area, theme().text());
        Clear.render(area, buf);

        // Outer border with the file name.
        let outer = Block::app_default().app_title(format!("Hex: {}", self.name));
        let inner = outer.inner(area);
        outer.render(area, buf);

        if self.detail.is_some() {
            self.render_detail(inner, buf);
            return;
        }

        // Two columns: left hex, right info+tree.
        let [left, right] =
            Layout::horizontal([Constraint::Fill(2), Constraint::Fill(3)]).areas(inner);

        self.render_hex(left, buf);
        let (_, bottom_h) = self.render_right(right, buf);
        // Stash tree height for keyboard navigation (subtract border rows).
        self.visible_tree_rows = bottom_h.saturating_sub(2);
    }

    fn handle(&mut self, event: KeyEvent) -> bool {
        if self.detail.is_some() {
            return self.handle_detail(event);
        }
        // Ctrl-g input mode takes precedence.
        if let Some(input) = self.goto_input.as_mut() {
            if input.handle(event) {
                return true;
            }
            return match (event.code, event.modifiers) {
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    let v = input.value().trim();
                    let off =
                        if let Some(rest) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
                            u64::from_str_radix(rest, 16).ok()
                        } else {
                            v.parse::<u64>().ok()
                        };
                    if let Some(o) = off {
                        self.jump_to(o);
                    }
                    self.goto_input = None;
                    true
                }
                (KeyCode::Esc, KeyModifiers::NONE) => {
                    self.goto_input = None;
                    true
                }
                _ => false,
            };
        }

        if event.code == KeyCode::Left && event.modifiers == KeyModifiers::ALT {
            self.return_reference();
            return true;
        }
        if event.modifiers == KeyModifiers::NONE {
            match event.code {
                KeyCode::Char('f') => {
                    self.follow_reference();
                    return true;
                }
                KeyCode::Char('d') => {
                    self.open_detail();
                    return true;
                }
                _ => {}
            }
        }
        let handled = match self.focus {
            Focus::Hex => self.handle_hex(event),
            Focus::Tree => self.handle_tree(event),
        };
        if handled {
            return true;
        }

        // Global keys.
        match (event.code, event.modifiers) {
            (KeyCode::Esc, KeyModifiers::NONE) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                Message::AppDismissHexViewer.enqueue();
                true
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                // Tab switches panes and syncs the selection:
                // - Hex → Tree: select the innermost tree node at the cursor.
                // - Tree → Hex: jump the cursor to the selected node's span.
                match self.focus {
                    Focus::Hex => {
                        if let Some(idx) = self
                            .layout
                            .tree()
                            .iter()
                            .enumerate()
                            .filter(|(_, n)| {
                                self.cursor.pos >= n.span.start && self.cursor.pos < n.span.end
                            })
                            .min_by_key(|(_, n)| n.span.end - n.span.start)
                            .map(|(i, _)| i)
                        {
                            self.tree_sel = idx;
                            self.ensure_node_visible(idx);
                        }
                        self.focus = Focus::Tree;
                    }
                    Focus::Tree => {
                        if let Some(n) = self.layout.tree().get(self.tree_sel) {
                            self.jump_to(n.span.start);
                        }
                        self.focus = Focus::Hex;
                    }
                }
                true
            }
            (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                // Ctrl-g: open offset input prefilled with current cursor.
                self.goto_input = Some(
                    Input::default()
                        .with_input_type(InputType::Any)
                        .with_value(format_offset(self.cursor.pos))
                        .with_hint("offset: 0x…".into()),
                );
                true
            }
            _ => false,
        }
    }

    fn update(&mut self, action: &Message) {
        if let Message::AppDismissHexViewer = action {
            // No-op here; App handles dismissal.
        }
    }
}

impl HexViewer {
    fn handle_hex(&mut self, event: KeyEvent) -> bool {
        match (event.code, event.modifiers) {
            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                self.cursor.left();
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                self.cursor.right();
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.cursor.up();
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.cursor.down();
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let page = (self.visible_rows as u64) * BYTES_PER_ROW as u64 / 2;
                self.cursor.page_up(page.max(BYTES_PER_ROW as u64));
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                let page = (self.visible_rows as u64) * BYTES_PER_ROW as u64 / 2;
                self.cursor.page_down(page.max(BYTES_PER_ROW as u64));
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) | (KeyCode::PageUp, KeyModifiers::NONE) => {
                let page = (self.visible_rows as u64) * BYTES_PER_ROW as u64;
                self.cursor.page_up(page.max(BYTES_PER_ROW as u64));
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('f'), KeyModifiers::CONTROL)
            | (KeyCode::PageDown, KeyModifiers::NONE) => {
                let page = (self.visible_rows as u64) * BYTES_PER_ROW as u64;
                self.cursor.page_down(page.max(BYTES_PER_ROW as u64));
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('0'), KeyModifiers::CONTROL) | (KeyCode::Home, KeyModifiers::NONE) => {
                self.cursor.row_start();
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('$'), KeyModifiers::NONE) | (KeyCode::End, KeyModifiers::NONE) => {
                self.cursor.row_end();
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                self.cursor.home();
                self.scroll = 0;
                true
            }
            (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
                self.cursor.end();
                self.ensure_cursor_visible();
                true
            }
            (KeyCode::Char('['), KeyModifiers::NONE) => {
                if let Some(start) = self
                    .regions
                    .iter()
                    .rev()
                    .filter(|r| r.span.start < self.cursor.pos)
                    .map(|r| r.span.start)
                    .next()
                {
                    self.jump_to(start);
                }
                true
            }
            (KeyCode::Char(']'), KeyModifiers::NONE) => {
                if let Some(start) = self
                    .regions
                    .iter()
                    .filter(|r| r.span.start > self.cursor.pos)
                    .map(|r| r.span.start)
                    .next()
                {
                    self.jump_to(start);
                }
                true
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                // Jump tree selection to the *innermost* node whose span
                // contains the cursor (smallest span = most specific region,
                // e.g. PageData instead of RowGroup).
                if let Some(idx) = self
                    .layout
                    .tree()
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| {
                        self.cursor.pos >= n.span.start && self.cursor.pos < n.span.end
                    })
                    .min_by_key(|(_, n)| n.span.end - n.span.start)
                    .map(|(i, _)| i)
                {
                    self.tree_sel = idx;
                    self.focus = Focus::Tree;
                    // Expand all ancestors so the selected node is visible.
                    self.ensure_node_visible(idx);
                }
                true
            }
            _ => false,
        }
    }

    fn handle_tree(&mut self, event: KeyEvent) -> bool {
        match (event.code, event.modifiers) {
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.tree_move(-1);
                true
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.tree_move(1);
                true
            }
            (KeyCode::PageUp, _) => {
                let h = self.visible_tree_rows.max(1) as i32;
                self.tree_move(-(h - 1));
                true
            }
            (KeyCode::PageDown, _) => {
                let h = self.visible_tree_rows.max(1) as i32;
                self.tree_move(h - 1);
                true
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let h = self.visible_tree_rows.max(1) as i32;
                self.tree_move(-((h - 1) / 2));
                true
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                let h = self.visible_tree_rows.max(1) as i32;
                self.tree_move((h - 1) / 2);
                true
            }
            (KeyCode::Home, _) => {
                let visible = self.visible_tree_indices();
                if let Some(&first) = visible.first() {
                    self.tree_sel = first;
                    self.tree_scroll = 0;
                }
                true
            }
            (KeyCode::End, _) => {
                let visible = self.visible_tree_indices();
                if let Some(&last) = visible.last() {
                    self.tree_sel = last;
                    let h = self.visible_tree_rows.max(1);
                    let last_pos = (visible.len() as u16).saturating_sub(1);
                    self.tree_scroll = last_pos.saturating_sub(h - 1);
                }
                true
            }
            (KeyCode::Char('='), KeyModifiers::NONE) | (KeyCode::Char('+'), KeyModifiers::NONE) => {
                // Toggle collapse/expand on the selected node.
                if self.collapsed.contains(&self.tree_sel) {
                    self.collapsed.remove(&self.tree_sel);
                } else if self
                    .layout
                    .tree()
                    .get(self.tree_sel)
                    .map(|n| n.collapsible)
                    .unwrap_or(false)
                {
                    self.collapsed.insert(self.tree_sel);
                }
                true
            }
            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                // Jump hex cursor to the selected node's span start.
                if let Some(n) = self.layout.tree().get(self.tree_sel) {
                    self.jump_to(n.span.start);
                    self.focus = Focus::Hex;
                }
                true
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                // Expand the selected node.
                self.collapsed.remove(&self.tree_sel);
                true
            }
            _ => false,
        }
    }

    fn render_hex(&mut self, area: Rect, buf: &mut ratatui::prelude::Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .style(theme().block())
            .title(Span::styled(
                if self.focus == Focus::Hex {
                    " Hex "
                } else {
                    " Hex (·) "
                },
                theme().block(),
            ));
        let inner = block.inner(area);
        block.render(area, buf);

        let row_h = inner.height as u64;
        self.visible_rows = inner.height;

        // Ensure scroll cursor visibility after size changes.
        self.ensure_cursor_visible();

        // Read the visible window.
        let window_start = self.scroll * BYTES_PER_ROW as u64;
        let window_len = (row_h as usize) * BYTES_PER_ROW;
        let bytes = match self.source.read_range(window_start, window_len) {
            Ok(b) => b,
            Err(e) => {
                self.error = Some(e.to_string());
                Vec::new()
            }
        };
        let hex_rows = rows(&bytes, window_start);

        let text_style = theme().text();
        let sub_style = theme().subtext();
        let hl_style = theme().text_highlighted().add_modifier(Modifier::REVERSED);

        // The region the cursor currently sits in — its bytes get a brighter
        // (bold) variant of the region color so the active region stands out
        // from the rest.
        let cursor_region = self.region_at(self.cursor.pos);

        let mut lines: Vec<Line> = Vec::with_capacity(hex_rows.len());
        for r in &hex_rows {
            let mut spans: Vec<Span> = Vec::with_capacity(4 + r.bytes.len());
            spans.push(Span::styled(format_offset(r.offset), sub_style));
            spans.push(Span::raw("  "));
            // Hex column.
            for (i, b) in r.bytes.iter().enumerate() {
                let abs = r.offset + i as u64;
                let sep = if i == BYTES_PER_ROW / 2 { " " } else { "" };
                let s = format!("{}{}", sep, format_byte(*b));
                if abs == self.cursor.pos {
                    spans.push(Span::styled(s, hl_style));
                } else if self.layout.is_recognized() {
                    let style = byte_style(abs, cursor_region, &self.regions, text_style);
                    spans.push(Span::styled(s, style));
                } else {
                    spans.push(Span::styled(s, text_style));
                }
                spans.push(Span::raw(" "));
            }
            // Pad short rows.
            for _ in r.bytes.len()..BYTES_PER_ROW {
                spans.push(Span::styled("   ", sub_style));
            }
            spans.push(Span::raw(" "));
            // ASCII column.
            for (i, b) in r.bytes.iter().enumerate() {
                let abs = r.offset + i as u64;
                let c = format_ascii(*b);
                if abs == self.cursor.pos {
                    spans.push(Span::styled(c.to_string(), hl_style));
                } else if self.layout.is_recognized() {
                    let style = byte_style(abs, cursor_region, &self.regions, text_style);
                    spans.push(Span::styled(c.to_string(), style));
                } else {
                    spans.push(Span::styled(c.to_string(), text_style));
                }
            }
            lines.push(Line::from(spans));
        }

        Paragraph::new(lines).style(text_style).render(inner, buf);

        // If Ctrl-g input is open, render it overlaid at the bottom of the hex pane.
        if let Some(input) = self.goto_input.as_mut() {
            let h = 3u16;
            let input_area = Rect {
                x: inner.x,
                y: inner.bottom().saturating_sub(h),
                width: inner.width,
                height: h.min(inner.height),
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .style(theme().block())
                .title(Span::styled(
                    " Go to offset (0x… / decimal) ",
                    theme().block(),
                ));
            let inner_in = block.inner(input_area);
            block.render(input_area, buf);
            input.render(inner_in, buf, FocusState::Focused);
        }
    }

    /// Render the right pane. Returns (top_height, _bottom_height) for layout bookkeeping.
    fn render_right(&mut self, area: Rect, buf: &mut ratatui::prelude::Buffer) -> (u16, u16) {
        let [top, bottom] =
            Layout::vertical([Constraint::Length(10), Constraint::Fill(1)]).areas(area);

        self.render_cursor_panel(top, buf);
        self.render_tree(bottom, buf);
        (top.height, bottom.height)
    }

    fn render_cursor_panel(&mut self, area: Rect, buf: &mut ratatui::prelude::Buffer) {
        let title = format!(" 0x{:08x} · cursor ", self.cursor.pos);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .style(theme().block())
            .title(Span::styled(title, theme().block()));
        let inner = block.inner(area);
        block.render(area, buf);

        let text_style = theme().text();
        let sub_style = theme().subtext();

        let mut lines: Vec<Line> = Vec::new();

        // Current byte value.
        if let Ok(window) = self.source.read_range(self.cursor.pos, 1) {
            if let Some(&b) = window.first() {
                lines.push(Line::from(vec![
                    Span::styled("offset : ", sub_style),
                    Span::styled(format_offset(self.cursor.pos), text_style),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("byte   : ", sub_style),
                    Span::styled(format!("0x{:02x} ({})", b, b), text_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("offset : ", sub_style),
                    Span::styled(format_offset(self.cursor.pos), text_style),
                ]));
            }
        }

        if self.layout.is_recognized() {
            if let Some(r) = self.region_at(self.cursor.pos) {
                lines.push(Line::from(vec![
                    Span::styled("region : ", sub_style),
                    Span::styled(r.kind_name, text_style),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("label  : ", sub_style),
                    Span::styled(r.label.clone(), text_style),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("span   : ", sub_style),
                    Span::styled(
                        format!("0x{:08x}..0x{:08x}", r.span.start, r.span.end),
                        text_style,
                    ),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("region : ", sub_style),
                    Span::styled("— (no structure here)", sub_style),
                ]));
            }
        } else if let Some(err) = self.layout.error() {
            lines.push(Line::from(vec![
                Span::styled("note   : ", sub_style),
                Span::styled(err.to_string(), sub_style),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("note   : ", sub_style),
                Span::styled("Not a recognized format", sub_style),
            ]));
        }

        if self.layout.is_recognized()
            && let Some(error) = self.layout.error()
        {
            lines.push(Line::from(format!("note   : {error}")));
        }
        Paragraph::new(lines)
            .style(text_style)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }

    fn render_tree(&mut self, area: Rect, buf: &mut ratatui::prelude::Buffer) {
        let title = if self.layout.is_recognized() {
            " Structure "
        } else {
            " Structure (none) "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .style(theme().block())
            .title(Span::styled(title, theme().block()));
        let inner = block.inner(area);
        block.render(area, buf);

        let text_style = theme().text();
        let sub_style = theme().subtext();
        let sel_style = theme().row_highlighted();

        if !self.layout.is_recognized() {
            let msg = match self.layout.error() {
                Some(e) => format!("Not a recognized format:\n{e}"),
                None => "Not a recognized format".to_string(),
            };
            Paragraph::new(msg)
                .style(sub_style)
                .wrap(Wrap { trim: true })
                .render(inner, buf);
            return;
        }

        // Render the visible (collapse-filtered) window of the flat tree.
        let visible = self.visible_tree_indices();
        let total = visible.len();
        let h = inner.height as usize;
        if total == 0 {
            Paragraph::new(self.layout.error().unwrap_or("(empty)"))
                .style(sub_style)
                .render(inner, buf);
            return;
        }
        // Position of the selection within the visible list.
        let sel_pos = visible
            .iter()
            .position(|&i| i == self.tree_sel)
            .unwrap_or(0);
        // Adjust scroll (in visible-row units) to keep selection visible.
        if sel_pos < self.tree_scroll as usize {
            self.tree_scroll = sel_pos as u16;
        } else if sel_pos >= (self.tree_scroll as usize) + h {
            self.tree_scroll = (sel_pos as u16).saturating_sub(h as u16 - 1);
        }
        let start = self.tree_scroll as usize;
        let end = (start + h).min(total);

        let mut lines: Vec<Line> = Vec::with_capacity(end - start);
        for &i in visible[start..end].iter() {
            let n = &self.layout.tree()[i];
            let indent = "  ".repeat(n.depth as usize);
            let marker = if n.collapsible {
                if self.collapsed.contains(&i) {
                    "▸ "
                } else {
                    "▾ "
                }
            } else {
                "  "
            };
            let span_label = if n.label.contains("@0x") {
                format!("{indent}{marker}{}", n.label)
            } else {
                format!(
                    "{indent}{marker}{}  [0x{:x}, 0x{:x}) · {} B",
                    n.label,
                    n.span.start,
                    n.span.end,
                    n.span.end.saturating_sub(n.span.start)
                )
            };
            let style = if i == self.tree_sel && self.focus == Focus::Tree {
                sel_style
            } else {
                text_style
            };
            lines.push(Line::from(Span::styled(span_label, style)));
        }
        Paragraph::new(lines).style(text_style).render(inner, buf);
    }
}

/// Compute the set of tree node indices that should be collapsed by default.
/// For Parquet, PageHeader nodes and the schema list are collapsed to keep the
/// initial view focused on the high-level structure. For Lance, the verbose
/// ColumnEncoding/ArrayEncoding protobuf subtrees are collapsed.
fn default_collapsed(layout: &FileLayout) -> std::collections::HashSet<usize> {
    let mut set = std::collections::HashSet::new();
    let tree = layout.tree();
    for (i, n) in tree.iter().enumerate() {
        if !n.collapsible {
            continue;
        }
        // Parquet: collapse page headers and the schema list.
        if n.label.starts_with("PageHeader ") || n.label.starts_with("schema[") {
            set.insert(i);
        }
        // Lance: collapse the protobuf column-metadata subtrees.
        // ColumnMetadata[N] (depth 1) and Page[N] nodes contain nested
        // ColumnEncoding/ArrayEncoding fields. Also collapse the "Column
        // Metadatas" parent and the encoding subtrees.
        if n.label.starts_with("ColumnMetadata[")
            || n.label.starts_with("Page[")
            || (layout.format() == crate::binary::layout::Format::Lance
                && n.label.starts_with("pages["))
            || n.label.starts_with("Column Metadatas")
            || n.label.starts_with("encoding (")
            || n.label.starts_with("direct (DirectEncoding)")
        {
            set.insert(i);
        }
    }
    set
}

/// Pick a style for a byte at `abs` based on which unified region it falls in.
/// Bytes in the cursor's current region are rendered bold (brighter) so the
/// active region stands out from other same-colored regions. Color indices
/// come precomputed in each `UnifiedRegion` (see `Layout::unified_regions`).
fn byte_style(
    abs: u64,
    cursor_region: Option<&UnifiedRegion>,
    regions: &[UnifiedRegion],
    default_style: Style,
) -> Style {
    let Some(r) = regions
        .iter()
        .filter(|r| abs >= r.span.start && abs < r.span.end)
        .min_by_key(|r| r.span.end - r.span.start)
    else {
        return default_style;
    };
    let mut s = theme().graph(r.color_idx);
    if let Some(cr) = cursor_region
        && cr.span.start == r.span.start
        && cr.span.end == r.span.end
    {
        s = s.add_modifier(Modifier::BOLD);
    }
    s
}

impl HexViewer {
    fn selected_offset(&self) -> u64 {
        if self.focus == Focus::Tree {
            self.layout
                .tree()
                .get(self.tree_sel)
                .map(|n| n.span.start)
                .unwrap_or(self.cursor.pos)
        } else {
            self.cursor.pos
        }
    }

    fn follow_reference(&mut self) {
        let offset = self.selected_offset();
        let references = match &self.layout {
            FileLayout::Parquet(layout) => layout.references.as_slice(),
            FileLayout::Lance(layout) => layout.references.as_slice(),
            FileLayout::Unknown { .. } => &[],
        };
        let target = references
            .iter()
            .filter(|r| r.source.contains(&offset))
            .min_by_key(|r| r.source.end - r.source.start)
            .map(|r| r.target.start)
            .or_else(|| {
                references
                    .iter()
                    .filter(|r| r.target.contains(&offset))
                    .min_by_key(|r| r.target.end - r.target.start)
                    .map(|r| r.source.start)
            });
        if let Some(target) = target {
            self.history.push(NavigationPosition {
                offset: self.cursor.pos,
                scroll: self.scroll,
                tree_sel: self.tree_sel,
                tree_scroll: self.tree_scroll,
                focus: self.focus,
                collapsed: self.collapsed.clone(),
            });
            self.jump_to(target);
            if let Some((idx, _)) = self
                .layout
                .tree()
                .iter()
                .enumerate()
                .filter(|(_, n)| n.span.contains(&target))
                .min_by_key(|(_, n)| n.span.end - n.span.start)
            {
                self.tree_sel = idx;
                self.ensure_node_visible(idx);
            }
        }
    }

    fn return_reference(&mut self) {
        if let Some(pos) = self.history.pop() {
            self.cursor.goto(pos.offset);
            self.scroll = pos.scroll;
            self.tree_sel = pos.tree_sel;
            self.tree_scroll = pos.tree_scroll;
            self.focus = pos.focus;
            self.collapsed = pos.collapsed;
        }
    }

    fn open_detail(&mut self) {
        let offset = self.selected_offset();
        let page = if let FileLayout::Parquet(p) = &self.layout {
            p.regions
                .iter()
                .find(|r| {
                    r.kind == crate::binary::parquet::RegionKind::PageHeader
                        && r.span.contains(&offset)
                })
                .map(|r| r.span.start)
                .or_else(|| {
                    p.tree
                        .iter()
                        .find(|n| n.label.starts_with("Page ") && n.span.contains(&offset))
                        .map(|n| n.span.start)
                })
        } else {
            None
        };
        if let Some(offset) = page {
            let result = if let Some(index) = self
                .detail_cache
                .iter()
                .position(|(key, _, _)| *key == offset)
            {
                let cached = self.detail_cache.remove(index).unwrap();
                let page = Arc::clone(&cached.1);
                self.detail_cache.push_back(cached);
                Ok(page)
            } else {
                self.source
                    .read_range(0, self.source.len() as usize)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| decode_page(&bytes, offset))
                    .map(|page| {
                        let cost = page
                            .decompressed
                            .capacity()
                            .saturating_add(
                                page.controls
                                    .iter()
                                    .chain(page.values.iter())
                                    .map(|s| s.capacity())
                                    .sum::<usize>(),
                            )
                            .saturating_add(
                                (page.controls.capacity() + page.values.capacity())
                                    .saturating_mul(std::mem::size_of::<String>()),
                            );
                        let page = Arc::new(page);
                        while self
                            .detail_cache
                            .iter()
                            .map(|x| x.2)
                            .sum::<usize>()
                            .saturating_add(cost)
                            > MAX_DETAIL_CACHE
                            && !self.detail_cache.is_empty()
                        {
                            self.detail_cache.pop_front();
                        }
                        if cost <= MAX_DETAIL_CACHE {
                            self.detail_cache
                                .push_back((offset, Arc::clone(&page), cost));
                        }
                        page
                    })
            };
            let (page, text) = match result {
                Ok(page) => (Some(page), vec![]),
                Err(error) => (None, vec![error]),
            };
            self.detail = Some(DetailView {
                title: format!("Page @0x{offset:x}"),
                field_range: None,
                page,
                text,
                tab: 0,
                scroll: [0; 3],
                horizontal: 0,
            });
        } else if let Some(node) = if self.focus == Focus::Tree {
            self.layout.tree().get(self.tree_sel)
        } else {
            self.layout
                .tree()
                .iter()
                .filter(|n| n.span.contains(&offset))
                .min_by_key(|n| n.span.end - n.span.start)
        } {
            self.detail = Some(DetailView {
                title: "Field details".into(),
                field_range: Some(node.span.clone()),
                page: None,
                text: vec![
                    node.label.clone(),
                    format!(
                        "file [0x{:x}, 0x{:x}) · {} B",
                        node.span.start,
                        node.span.end,
                        node.span.end.saturating_sub(node.span.start)
                    ),
                ],
                tab: 0,
                scroll: [0; 3],
                horizontal: 0,
            });
        }
    }

    fn handle_detail(&mut self, event: KeyEvent) -> bool {
        if event.code == KeyCode::Esc || event.code == KeyCode::Char('q') {
            self.detail = None;
            return true;
        }
        let Some(detail) = &mut self.detail else {
            return false;
        };
        match event.code {
            KeyCode::Tab => {
                detail.tab = (detail.tab + 1) % 3;
                detail.horizontal = 0;
            }
            KeyCode::BackTab => {
                detail.tab = (detail.tab + 2) % 3;
                detail.horizontal = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                detail.scroll[detail.tab] = detail.scroll[detail.tab].saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                detail.scroll[detail.tab] = detail.scroll[detail.tab].saturating_add(1)
            }
            KeyCode::PageUp => {
                detail.scroll[detail.tab] =
                    detail.scroll[detail.tab].saturating_sub(self.visible_rows.max(1) as usize)
            }
            KeyCode::PageDown => {
                detail.scroll[detail.tab] =
                    detail.scroll[detail.tab].saturating_add(self.visible_rows.max(1) as usize)
            }
            KeyCode::Home => {
                detail.scroll[detail.tab] = 0;
                detail.horizontal = 0;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                detail.horizontal = detail.horizontal.saturating_add(8)
            }
            KeyCode::Left | KeyCode::Char('h') => {
                detail.horizontal = detail.horizontal.saturating_sub(8)
            }
            _ => {}
        }
        true
    }

    fn render_detail(&mut self, area: Rect, buf: &mut ratatui::prelude::Buffer) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        let tab = if detail.page.is_some() {
            ["Decompressed hex", "Controls", "Decoded values"][detail.tab]
        } else {
            ["Field value", "Encoded field hex", "Field value"][detail.tab]
        };
        let block = Block::default().borders(Borders::ALL).title(format!(
            " {} · {} · Tab switch · Esc return ",
            detail.title, tab
        ));
        let inner = block.inner(area);
        block.render(area, buf);
        self.visible_rows = inner.height;
        let header = detail
            .page
            .as_ref()
            .map(|page| {
                format!(
                    "Source file [0x{:x}, 0x{:x}); offsets below are in decompressed page bytes",
                    page.source_span.start, page.source_span.end
                )
            })
            .unwrap_or_else(|| "Field details · Tab: full encoded bytes (including Thrift header) · ←/→: long values".into());
        let mut lines = vec![Line::from(header)];
        let h = inner.height.saturating_sub(1) as usize;
        let count = match &detail.page {
            Some(page) => match detail.tab {
                0 => page.decompressed.len().div_ceil(BYTES_PER_ROW),
                1 => page.controls.len(),
                _ => page.values.len(),
            },
            None => {
                if detail.tab == 1 {
                    detail
                        .field_range
                        .as_ref()
                        .map(|r| (r.end - r.start).div_ceil(BYTES_PER_ROW as u64) as usize)
                        .unwrap_or(detail.text.len())
                } else {
                    detail.text.len()
                }
            }
        };
        detail.scroll[detail.tab] = detail.scroll[detail.tab].min(count.saturating_sub(h.max(1)));
        let start = detail.scroll[detail.tab];
        if let Some(page) = &detail.page {
            if detail.tab == 0 {
                let byte_start = start * BYTES_PER_ROW;
                let byte_end = byte_start
                    .saturating_add(h * BYTES_PER_ROW)
                    .min(page.decompressed.len());
                for row in rows(
                    &page.decompressed[byte_start.min(byte_end)..byte_end],
                    byte_start as u64,
                ) {
                    lines.push(Line::from(format!(
                        "page+0x{:08x}  {}  {}",
                        row.offset,
                        row.bytes
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<Vec<_>>()
                            .join(" "),
                        row.bytes
                            .iter()
                            .map(|b| format_ascii(*b))
                            .collect::<String>()
                    )));
                }
            } else {
                let values = if detail.tab == 1 {
                    &page.controls
                } else {
                    &page.values
                };
                lines.extend(
                    values
                        .iter()
                        .skip(start)
                        .take(h)
                        .map(|v| Line::from(v.chars().skip(detail.horizontal).collect::<String>())),
                );
            }
        } else if detail.tab == 1
            && let Some(range) = detail.field_range.as_ref()
        {
            let offset = range
                .start
                .saturating_add((start * BYTES_PER_ROW) as u64)
                .min(range.end);
            let length = (h * BYTES_PER_ROW).min((range.end - offset) as usize);
            match self.source.read_range(offset, length) {
                Ok(bytes) => {
                    for row in rows(&bytes, offset) {
                        lines.push(Line::from(format!(
                            "file+0x{:08x}  {}  {}",
                            row.offset,
                            row.bytes
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<Vec<_>>()
                                .join(" "),
                            row.bytes
                                .iter()
                                .map(|b| format_ascii(*b))
                                .collect::<String>()
                        )));
                    }
                }
                Err(error) => lines.push(Line::from(error.to_string())),
            }
        } else {
            lines.extend(
                detail
                    .text
                    .iter()
                    .skip(start)
                    .take(h)
                    .map(|v| Line::from(v.chars().skip(detail.horizontal).collect::<String>())),
            );
        }
        Paragraph::new(lines)
            .style(theme().text())
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary::parquet::{MetaNode, ParquetLayout, Reference};

    fn navigation_viewer() -> HexViewer {
        let mut viewer = HexViewer::from_bytes("test", vec![0; 100]);
        let layout = ParquetLayout {
            is_parquet: true,
            tree: vec![
                MetaNode {
                    depth: 0,
                    label: "data".into(),
                    span: 10..20,
                    collapsible: false,
                },
                MetaNode {
                    depth: 0,
                    label: "metadata".into(),
                    span: 70..90,
                    collapsible: true,
                },
                MetaNode {
                    depth: 1,
                    label: "offset".into(),
                    span: 75..77,
                    collapsible: false,
                },
            ],
            references: vec![Reference {
                source: 75..77,
                target: 10..20,
            }],
            ..ParquetLayout::default()
        };
        viewer.layout = FileLayout::Parquet(layout);
        viewer
    }

    #[test]
    fn references_return_to_exact_navigation_state() {
        let mut viewer = navigation_viewer();
        viewer.focus = Focus::Tree;
        viewer.tree_sel = 2;
        viewer.cursor.goto(76);
        viewer.scroll = 3;
        viewer.follow_reference();
        assert_eq!(viewer.cursor.pos, 10);
        assert_eq!(viewer.tree_sel, 0);
        viewer.handle(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(viewer.cursor.pos, 76);
        assert_eq!(viewer.tree_sel, 2);
        assert_eq!(viewer.scroll, 3);
        assert_eq!(viewer.focus, Focus::Tree);
    }

    #[test]
    fn detail_navigation_does_not_move_underlying_file() {
        let mut viewer = navigation_viewer();
        viewer.focus = Focus::Tree;
        viewer.tree_sel = 2;
        viewer.cursor.goto(76);
        viewer.open_detail();
        assert!(viewer.detail.is_some());
        viewer.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        viewer.handle(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        viewer.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(viewer.detail.is_none());
        assert_eq!(viewer.cursor.pos, 76);
        assert_eq!(viewer.tree_sel, 2);
    }

    #[test]
    fn field_details_read_past_label_preview() {
        let mut viewer = HexViewer::from_bytes("binary", vec![0xab; 512]);
        viewer.layout = FileLayout::Parquet(ParquetLayout {
            tree: vec![MetaNode {
                depth: 0,
                label: "binary preview".into(),
                span: 0..512,
                collapsible: false,
            }],
            ..ParquetLayout::default()
        });
        viewer.focus = Focus::Tree;
        viewer.open_detail();
        let detail = viewer.detail.as_mut().unwrap();
        detail.tab = 1;
        detail.scroll[1] = 25;
        let area = Rect::new(0, 0, 140, 6);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        viewer.render_detail(area, &mut buffer);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("file+0x00000190"));
        assert!(text.contains("ab ab ab"));
    }

    #[test]
    fn offset_input_accepts_detail_and_reference_shortcut_characters() {
        let mut viewer = navigation_viewer();
        viewer.handle(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        viewer.handle(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        viewer.handle(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(viewer.detail.is_none());
        assert!(viewer.goto_input.as_ref().unwrap().value().contains("df"));
    }

    #[test]
    fn large_file_skips_structure_before_full_read() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_STRUCTURE_BYTES + 1).unwrap();
        let viewer = HexViewer::open_path(file.path()).unwrap();
        assert!(matches!(viewer.layout, FileLayout::Unknown { .. }));
        assert!(
            viewer
                .layout
                .error()
                .unwrap()
                .contains("Structure parsing skipped")
        );
        assert!(viewer.regions.is_empty());
    }

    #[test]
    fn detail_render_marks_derived_address_space() {
        let mut viewer = navigation_viewer();
        viewer.detail = Some(DetailView {
            title: "Page".into(),
            field_range: None,
            page: Some(Arc::new(PageDetails {
                source_span: 10..20,
                decompressed: vec![1, 2],
                controls: vec!["bit_width=2".into()],
                values: vec!["value=1".into()],
            })),
            text: vec![],
            tab: 0,
            scroll: [0; 3],
            horizontal: 0,
        });
        let area = Rect::new(0, 0, 140, 12);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        viewer.render_detail(area, &mut buffer);
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Source file [0xa, 0x14)"));
        assert!(text.contains("page+0x00000000"));
        viewer.handle_detail(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        viewer.render_detail(area, &mut buffer);
        assert!(
            buffer
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("bit_width=2")
        );
    }

    #[test]
    fn equal_span_parent_is_expanded_for_reference_selection() {
        let mut viewer = navigation_viewer();
        if let FileLayout::Parquet(p) = &mut viewer.layout {
            p.tree[1].span = 75..77;
        }
        viewer.collapsed.insert(1);
        viewer.ensure_node_visible(2);
        assert!(!viewer.collapsed.contains(&1));
    }
    #[test]
    fn lance_buffer_reference_navigation_and_field_details() {
        let bytes = include_bytes!("../../tests/fixtures/dentry.lance");
        let mut viewer = HexViewer::from_bytes("dentry.lance", bytes.to_vec());
        let (source, target) = match &viewer.layout {
            FileLayout::Lance(layout) => {
                assert!(layout.error.is_none(), "{:?}", layout.error);
                let reference = layout
                    .references
                    .iter()
                    .find(|r| r.target.start == 0)
                    .unwrap();
                (reference.source.start, reference.target.start)
            }
            _ => panic!("expected Lance"),
        };
        viewer.cursor.goto(source);
        viewer.follow_reference();
        assert_eq!(viewer.cursor.pos, target);
        viewer.return_reference();
        assert_eq!(viewer.cursor.pos, source);
        viewer.open_detail();
        assert!(viewer.detail.is_some());
        assert!(viewer.detail.as_ref().unwrap().page.is_none());
    }
}
