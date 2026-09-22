mod browser;
mod files_view;

use files_view::FilesView;
use viewkit::prelude::*;

struct FilesApp;

impl App for FilesApp {
    type Body = FilesView;

    fn new() -> Self {
        Self
    }

    fn window(&self) -> WindowOptions {
        WindowOptions::new("Files")
            .size(920.0, 640.0)
            .resizable(true)
    }

    fn body(&self, _context: &ViewContext) -> Self::Body {
        FilesView::new()
    }
}

fn main() -> Result<(), ViewKitError> {
    run::<FilesApp>()
}
