//! Rendering regression tests, organized by behavior.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;
use crate::app::MultilineInput;
use cagent_agent::presentation::{
    ExplorationActivity, ExplorationActivityKind, ToolActivityGroup, ToolActivityStatus,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier};
use ratatui_image::Resize;
use ratatui_image::picker::{Picker, ProtocolType};

mod images;
mod paths;
mod transcript;
