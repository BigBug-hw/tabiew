use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders, Clear, Widget},
};

use crate::{
    handler::message::Message,
    tui::{
        app_default::AppDefault,
        component::{Component, FocusState},
        popups::pickers::file_picker::FilePicker,
    },
};

/// A file picker that opens its selection in the hex viewer.
///
/// Wraps [`FilePicker`]; we only handle `Enter` (file → hex viewer, dir → let
/// the picker keep navigating) and `Esc`. `Tab` and suggestion navigation are
/// already handled by the inner picker.
#[derive(Debug)]
pub struct HexFilePicker {
    picker: FilePicker,
}

impl Default for HexFilePicker {
    fn default() -> Self {
        Self {
            picker: FilePicker::default().with_title("Open in Hex Viewer"),
        }
    }
}

impl Component for HexFilePicker {
    fn render(&mut self, area: Rect, buf: &mut ratatui::prelude::Buffer, focus: FocusState) {
        // Centered popup.
        let h = 7u16.min(area.height);
        let w = 64u16.min(area.width);
        let [popup] = ratatui::layout::Layout::vertical([ratatui::layout::Constraint::Length(h)])
            .flex(ratatui::layout::Flex::Center)
            .areas(area);
        let [popup] = ratatui::layout::Layout::horizontal([ratatui::layout::Constraint::Length(w)])
            .flex(ratatui::layout::Flex::Center)
            .areas(popup);
        Clear.render(popup, buf);
        let block = Block::app_default();
        let inner = block.inner(popup);
        block.render(popup, buf);
        let _ = Borders::ALL; // silence unused import if Block::app_default already borders
        self.picker.render(inner, buf, focus);
    }

    fn handle(&mut self, event: crossterm::event::KeyEvent) -> bool {
        if self.picker.handle(event) {
            return true;
        }
        match (event.code, event.modifiers) {
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let path: PathBuf = self.picker.path();
                if path.is_file() {
                    Message::AppShowHexViewer(Some(path)).enqueue();
                    Message::AppDismissOverlay.enqueue();
                } else {
                    Message::AppShowToast("Not a file".into()).enqueue();
                }
                true
            }
            (KeyCode::Esc, KeyModifiers::NONE) => {
                Message::AppDismissOverlay.enqueue();
                true
            }
            _ => false,
        }
    }
}
