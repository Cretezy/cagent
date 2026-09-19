//! Color- and layout-neutral projections shared by every frontend.

mod activity;
mod attachments;
mod export;
mod file_view;
mod history;
mod keybindings;
mod markdown;
mod path_references;
mod permissions;
mod picker;
mod questions;
mod settings;
mod statusline;
mod streaming;
mod supervised_work;
mod time;

pub use activity::*;
pub use attachments::*;
pub use export::*;
pub use file_view::*;
pub use history::*;
pub use keybindings::*;
pub use markdown::*;
pub use path_references::*;
pub use permissions::*;
pub use picker::*;
pub use questions::*;
pub use settings::*;
pub use statusline::*;
pub use streaming::*;
pub use supervised_work::*;
pub use time::*;
