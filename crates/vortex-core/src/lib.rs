//! Vortex core: all logic. Must never depend on gtk4, glib, gio or libadwaita.

pub mod backup;
pub mod db;
pub mod delete;
pub mod desktop;
pub mod diagnose;
pub mod error;
pub mod install;
pub mod launch;
pub mod logs;
pub mod paths;
pub mod process;
pub mod runner;
pub mod runtime;
pub mod runtime_ge;
pub mod source;
pub mod winetricks;
