// SPDX-License-Identifier: MIT
//! Versioned preferences and transactional persistence. No GPUI dependency here.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    WarmPaper,
}

impl Theme {
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::WarmPaper];
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::WarmPaper => "Warm Paper",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u32,
    pub theme: Theme,
    pub studio: bool,
    pub reduced_motion: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            theme: Theme::Dark,
            studio: false,
            reduced_motion: true,
        }
    }
}

/// Only the compositor adapter knows how to read/apply desktop motion.
pub trait Motion {
    fn reduced(&self) -> Result<bool, String>;
    fn set_reduced(&self, reduced: bool) -> Result<(), String>;
}

#[derive(Clone)]
pub struct Store {
    pub path: PathBuf,
}

impl Store {
    pub fn for_user() -> Result<Self, String> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| PathBuf::from(p).is_absolute())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .ok_or("Cannot find your configuration directory.")?;
        Ok(Self {
            path: base.join("crowbaros/appearance.json"),
        })
    }

    pub fn load(&self) -> Result<Option<Preferences>, String> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("Cannot read preferences: {e}")),
        };
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 65536 {
            return Err("Preferences exceed the 64 KiB limit; file left unchanged.".into());
        }
        let prefs: Preferences = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Preferences are invalid; file left unchanged: {e}"))?;
        if prefs.schema_version != 1 {
            return Err(format!(
                "Preferences use schema {}. This version supports schema 1; file left unchanged.",
                prefs.schema_version
            ));
        }
        Ok(Some(prefs))
    }

    fn write_error(&self, error: std::io::Error) -> String {
        let folder = self.path.parent().unwrap_or(&self.path).display();
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            format!(
                "Cannot write settings in {folder}. Restore write permission for your account, then choose Save changes again."
            )
        } else {
            format!(
                "Cannot save settings in {folder}: {error}. Check available space and write access, then try Save changes again."
            )
        }
    }

    pub fn save(
        &self,
        draft: &Preferences,
        expected: &Option<Preferences>,
        motion: &impl Motion,
    ) -> Result<(), String> {
        if draft.schema_version != 1 {
            return Err("Unsupported settings version.".into());
        }
        let parent = self.path.parent().ok_or("Invalid preference path.")?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|e| self.write_error(e))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(parent.join("appearance.lock"))
            .map_err(|e| self.write_error(e))?;
        lock.try_lock()
            .map_err(|_| "Another settings save is in progress. Try again.".to_string())?;
        if &self.load()? != expected {
            return Err("Preferences changed in another window. Use Reload saved, then make your changes again.".into());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let temp = parent.join(format!(".appearance-{}-{stamp}.tmp", std::process::id()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
                .map_err(|e| self.write_error(e))?;
            let bytes = serde_json::to_vec_pretty(draft).map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|e| self.write_error(e))?;
            let previous = motion.reduced()?;
            if let Err(error) = motion.set_reduced(draft.reduced_motion) {
                return match motion.set_reduced(previous) {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(format!(
                        "{error} Could not restore desktop motion: {rollback}"
                    )),
                };
            }
            if let Err(e) = fs::rename(&temp, &self.path) {
                let rollback = motion.set_reduced(previous);
                return Err(match rollback {
                    Ok(()) => format!("Could not save preferences; motion was restored: {e}"),
                    Err(r) => format!(
                        "Could not save preferences ({e}); motion could not be restored ({r})."
                    ),
                });
            }
            // The atomic rename is the commit point. A sync failure is logged,
            // never presented as an unsaved transaction that already committed.
            if let Err(e) = File::open(parent).and_then(|f| f.sync_all()) {
                log::warn!("Settings committed, but directory sync failed: {e}");
            }
            Ok(())
        })();
        let _ = fs::remove_file(temp);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Fake {
        reduced: Cell<bool>,
        fail: bool,
    }
    impl Motion for Fake {
        fn reduced(&self) -> Result<bool, String> {
            Ok(self.reduced.get())
        }
        fn set_reduced(&self, value: bool) -> Result<(), String> {
            if self.fail {
                Err("compositor unavailable".into())
            } else {
                self.reduced.set(value);
                Ok(())
            }
        }
    }
    struct Fixture(Store);
    impl Fixture {
        fn new() -> Self {
            Self(Store {
                path: std::env::temp_dir()
                    .join(format!(
                        "studio-test-{}-{}",
                        std::process::id(),
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    ))
                    .join("appearance.json"),
            })
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.path.parent().unwrap());
        }
    }
    #[test]
    fn saves_roundtrip_and_rejects_stale_window() {
        let f = Fixture::new();
        let motion = Fake {
            reduced: Cell::new(false),
            fail: false,
        };
        assert_eq!(f.0.load().unwrap(), None);
        let first = Preferences::default();
        f.0.save(&first, &None, &motion).unwrap();
        assert_eq!(f.0.load().unwrap(), Some(first.clone()));
        assert!(motion.reduced.get());
        let second = Preferences {
            theme: Theme::WarmPaper,
            studio: true,
            ..first.clone()
        };
        f.0.save(&second, &Some(first.clone()), &motion).unwrap();
        assert!(
            f.0.save(&first, &Some(first.clone()), &motion)
                .unwrap_err()
                .contains("another window")
        );
        assert_eq!(f.0.load().unwrap(), Some(second));
    }
    #[test]
    fn failed_compositor_does_not_write_preferences() {
        let f = Fixture::new();
        let motion = Fake {
            reduced: Cell::new(false),
            fail: true,
        };
        assert!(f.0.save(&Preferences::default(), &None, &motion).is_err());
        assert_eq!(f.0.load().unwrap(), None);
        assert!(
            !fs::read_dir(f.0.path.parent().unwrap()).unwrap().any(|p| p
                .unwrap()
                .path()
                .extension()
                .is_some_and(|s| s == "tmp"))
        );
    }
    #[test]
    fn refuses_unknown_schema_and_corruption() {
        let f = Fixture::new();
        fs::create_dir_all(f.0.path.parent().unwrap()).unwrap();
        let prefs = Preferences {
            schema_version: 99,
            ..Preferences::default()
        };
        fs::write(&f.0.path, serde_json::to_vec(&prefs).unwrap()).unwrap();
        assert!(f.0.load().unwrap_err().contains("schema 99"));
        fs::write(&f.0.path, b"broken json").unwrap();
        assert!(f.0.load().unwrap_err().contains("invalid"));
        let motion = Fake {
            reduced: Cell::new(false),
            fail: false,
        };
        assert!(f.0.save(&Preferences::default(), &None, &motion).is_err());
        assert_eq!(fs::read(&f.0.path).unwrap(), b"broken json");
    }
    #[test]
    fn failed_rename_restores_previous_motion() {
        let f = Fixture::new();
        struct Sabotage {
            path: PathBuf,
            reduced: Cell<bool>,
            calls: Cell<u32>,
        }
        impl Motion for Sabotage {
            fn reduced(&self) -> Result<bool, String> {
                Ok(self.reduced.get())
            }
            fn set_reduced(&self, v: bool) -> Result<(), String> {
                self.reduced.set(v);
                self.calls.set(self.calls.get() + 1);
                if self.calls.get() == 1 {
                    fs::create_dir(&self.path).unwrap();
                }
                Ok(())
            }
        }
        let motion = Sabotage {
            path: f.0.path.clone(),
            reduced: Cell::new(false),
            calls: Cell::new(0),
        };
        assert!(
            f.0.save(&Preferences::default(), &None, &motion)
                .unwrap_err()
                .contains("motion was restored")
        );
        assert!(!motion.reduced.get());
        assert_eq!(motion.calls.get(), 2);
    }
}
