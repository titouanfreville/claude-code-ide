//! State for the **Add Data Source** modal.
//!
//! The modal is a window-level dialog hosted by the [`Workspace`](crate::views::workspace)
//! (the right dock is too narrow for a DataGrip-width form), mirroring the run-config modal:
//! this module owns the pure form *state* + the [`DataSource`] it assembles, while the
//! Workspace renders it and drives Test / Save. Keeping the assembly here makes it unit-testable.

use std::path::PathBuf;

use gpui::{App, AppContext, Entity, Window};
use gpui_component::input::InputState;

use super::db_source::{self, DataSource, PgConfig};

/// Which driver the operator is adding.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Sqlite,
    Postgres,
}

/// The result of the last "Test Connection" click.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TestState {
    #[default]
    Idle,
    Testing,
    Ok(String),
    Err(String),
}

/// The Add-Data-Source form's live state (inputs + chosen driver + last test result).
pub struct AddSourceForm {
    pub driver: Driver,
    pub test: TestState,
    /// SQLite path chosen via the native picker (no text input → no `set_value` dance).
    pub sqlite_path: Option<PathBuf>,
    /// Optional display-label override (defaults to `db@host`).
    pub name: Entity<InputState>,
    pub host: Entity<InputState>,
    pub port: Entity<InputState>,
    pub database: Entity<InputState>,
    pub user: Entity<InputState>,
    pub password: Entity<InputState>,
    /// A raw DSN that overrides the structured fields when non-empty (DataGrip's URL row).
    pub url: Entity<InputState>,
}

impl AddSourceForm {
    /// A fresh Postgres-defaulted form. Inputs are built eagerly (a `Window` is in hand).
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        fn input(window: &mut Window, cx: &mut App, ph: &str) -> Entity<InputState> {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        }
        Self {
            driver: Driver::Postgres,
            test: TestState::Idle,
            sqlite_path: None,
            name: input(window, cx, "optional label"),
            host: input(window, cx, "localhost"),
            port: input(window, cx, "5432"),
            database: input(window, cx, "dbname"),
            user: input(window, cx, "postgres"),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("password")
                    .masked(true)
            }),
            url: input(window, cx, "postgres://user:pass@host:5432/dbname"),
        }
    }

    /// Build the [`DataSource`] the form currently describes, or `None` when the required
    /// fields are still blank (nothing to test/save yet).
    pub fn current_source(&self, cx: &App) -> Option<DataSource> {
        match self.driver {
            Driver::Sqlite => self.sqlite_path.clone().map(DataSource::Sqlite),
            Driver::Postgres => {
                let val = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
                let name = val(&self.name);
                let url = val(&self.url);
                let cfg = if !url.is_empty() {
                    let label = if name.is_empty() {
                        db_source::pg_label(&url)
                    } else {
                        name
                    };
                    PgConfig { label, dsn: url }
                } else {
                    let cfg = PgConfig::from_parts(
                        &val(&self.host),
                        &val(&self.port),
                        &val(&self.database),
                        &val(&self.user),
                        &val(&self.password),
                    );
                    match name.is_empty() {
                        true => cfg,
                        false => PgConfig { label: name, ..cfg },
                    }
                };
                // Reject the trivial "nothing entered" DSN.
                (cfg.dsn != "postgresql://localhost/").then_some(DataSource::Postgres(cfg))
            }
        }
    }
}
