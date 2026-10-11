//! The demo's session: hot-reloaded envelope files in, printable text out.
//!
//! The session owns no clock and prints nothing. The caller passes the time
//! and prints what comes back, so the whole thing is testable with a fake
//! clock. Native targets only: it needs the file watcher.

use std::fmt::Write as _;
use std::path::Path;

use st_data::{ContentHash, DataSetEntry, Diagnostic, Loader, data_set_hash};
use st_game::{HotData, ReloadError};
use st_time::Nanos;

use crate::envelope::SpaceEnvelope;
use crate::{figures, scenario};

/// While a reload error stands, it is printed again this often.
pub const REMINDER_EVERY: Nanos = Nanos::from_secs(10);

/// A directory of envelope files, watched and shown.
///
/// ```no_run
/// use phase1_demo::Session;
/// use st_time::Nanos;
///
/// let mut session = Session::open("assets/space_envelope")?;
/// print!("{}", session.report("started"));
/// // Once per frame, with a monotonic time:
/// if let Some(text) = session.poll(Nanos::from_millis(16)) {
///     print!("{text}");
/// }
/// # Ok::<(), String>(())
/// ```
pub struct Session {
    root: String,
    hot: HotData<SpaceEnvelope>,
    /// The latest reload error, kept until a valid edit arrives.
    last_error: Option<ReloadError>,
    last_reminder: Nanos,
}

impl Session {
    /// Loads every `.ron` file under `root` and starts watching it.
    ///
    /// # Errors
    ///
    /// The readable error text (one line per problem, with file paths) when a
    /// file cannot be loaded. There is no earlier value to fall back on.
    pub fn open(root: impl AsRef<Path>) -> Result<Session, String> {
        let root = root.as_ref();
        let shown = display_root(root);
        let hot = HotData::open(root, Loader::new()).map_err(|e| render_error(&shown, &e))?;
        Ok(Session {
            root: shown,
            hot,
            last_error: None,
            last_reminder: Nanos::ZERO,
        })
    }

    /// Does one slice of watcher work; call once per frame. Returns text to
    /// print when something happened: a good edit, a bad edit, or the reminder
    /// that a bad edit still stands.
    pub fn poll(&mut self, now: Nanos) -> Option<String> {
        if let Some(report) = self.hot.poll(now) {
            self.last_reminder = now;
            let mut head = String::new();
            if let Some(error) = report.error {
                let _ = writeln!(
                    head,
                    "\n--- EDIT REJECTED: the old values stay in use (data generation {}) ---",
                    self.hot.generation()
                );
                self.last_error = Some(error);
            } else if let Some(reloaded) = &report.reloaded {
                self.last_error = None;
                let names: Vec<&str> = reloaded.events.iter().map(|e| e.source.as_str()).collect();
                let _ = writeln!(
                    head,
                    "\n--- RELOADED (data generation {}): {} changed ---",
                    reloaded.generation,
                    names.join(", ")
                );
            }
            return Some(format!("{head}{}", self.body()));
        }
        if self.last_error.is_some() && now.saturating_sub(self.last_reminder) >= REMINDER_EVERY {
            self.last_reminder = now;
            return Some(format!(
                "\n--- STILL WAITING for a valid edit; the old values are in use ---\n{}",
                self.error_text()
            ));
        }
        None
    }

    /// The full printout for the current state, headed by `title`.
    pub fn report(&self, title: &str) -> String {
        format!(
            "--- {title} (data generation {}) ---\n{}",
            self.hot.generation(),
            self.body()
        )
    }

    /// The last reload error, if one stands.
    pub fn last_error(&self) -> Option<&ReloadError> {
        self.last_error.as_ref()
    }

    /// The data generation: 0 at the start, plus one per accepted edit.
    pub fn generation(&self) -> u64 {
        self.hot.generation()
    }

    fn error_text(&self) -> String {
        self.last_error
            .as_ref()
            .map(|e| format!("{}\n", render_error(&self.root, e)))
            .unwrap_or_default()
    }

    /// Values, figures and replay of every file, then the standing error.
    fn body(&self) -> String {
        let snapshot = self.hot.snapshot();
        let mut out = String::new();
        let mut entries = Vec::new();
        let stale = if self.last_error.is_some() {
            " (OLD values, from the last good save)"
        } else {
            ""
        };
        for (name, env) in snapshot.iter() {
            let hash: Option<ContentHash> = snapshot.loaded(name).map(|l| l.hash);
            let _ = writeln!(out, "{}/{name}{stale}", self.root);
            let _ = writeln!(out, "  validated values:");
            out.push_str(&figures::values(env));
            let _ = writeln!(out, "  what that feels like:");
            out.push_str(&figures::feel(env));
            let _ = writeln!(out, "  replay (two ships fly edge to edge, deterministic):");
            match scenario::replay(env) {
                Ok(r) => {
                    let _ = writeln!(
                        out,
                        "    normal ship {}, cruise ship {}, {} ticks at 120 Hz from seed {}",
                        figures::duration(r.normal_crossing_s),
                        figures::duration(r.cruise_crossing_s),
                        figures::number(r.ticks as f64),
                        env.replay_seed
                    );
                    let _ = writeln!(out, "    replay hash: {:016x}", r.hash);
                }
                Err(e) => {
                    let _ = writeln!(out, "    no replay: {e}");
                }
            }
            if let Some(hash) = hash {
                entries.push(DataSetEntry {
                    kind: "space_envelope".to_owned(),
                    source: name.to_owned(),
                    hash,
                });
            }
        }
        if let Ok(hash) = data_set_hash(&entries) {
            let _ = writeln!(out, "  data hash: {hash}");
        }
        if self.last_error.is_some() {
            out.push_str(
                "\n!! LAST RELOAD ERROR (still standing; the values above are the old ones):\n",
            );
            out.push_str(&self.error_text());
            out.push_str("!! Fix the file and save it; the demo picks it up by itself.\n");
        }
        out
    }
}

/// The root as the user gave it, with `/` separators, for messages.
fn display_root(root: &Path) -> String {
    let text = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if text.is_empty() {
        ".".to_owned()
    } else {
        text
    }
}

/// One readable block: every diagnostic with its file path (the root plus the
/// source name), line and column, rule and help.
fn render_error(root: &str, error: &ReloadError) -> String {
    error
        .diagnostics()
        .iter()
        .map(|d| {
            let mut shown: Diagnostic = d.clone();
            shown.source = format!("{root}/{}", d.source);
            format!("{shown:#}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
