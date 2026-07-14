use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::session::Session;

pub struct Cache {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    store: Store,
    accessed: HashSet<String>,
    dirty: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    sessions: HashMap<String, Entry<Option<Session>>>,
    titles: HashMap<String, Entry<Option<String>>>,
}

#[derive(Serialize, Deserialize)]
struct Entry<T> {
    len: u64,
    modified_ms: i64,
    value: T,
}

impl Cache {
    pub fn load(path: Option<PathBuf>) -> Cache {
        let store = path
            .as_deref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Cache {
            path,
            inner: Mutex::new(Inner {
                store,
                ..Inner::default()
            }),
        }
    }

    pub fn default_path() -> PathBuf {
        std::env::temp_dir().join("ap/sessions.json")
    }

    pub fn sessions(
        &self,
        files: Vec<PathBuf>,
        parse: impl Fn(&Path) -> Option<Session> + Sync,
    ) -> Vec<Session> {
        let stamped: Vec<(PathBuf, u64, i64)> = files
            .into_par_iter()
            .filter_map(|path| stamp(&path).map(|(len, modified_ms)| (path, len, modified_ms)))
            .collect();
        let mut sessions = Vec::new();
        let mut misses = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap();
            for (path, len, modified_ms) in stamped {
                let key = path.to_string_lossy().into_owned();
                inner.accessed.insert(key.clone());
                match inner.store.sessions.get(&key) {
                    Some(entry) if entry.len == len && entry.modified_ms == modified_ms => {
                        sessions.extend(entry.value.clone());
                    }
                    _ => misses.push((path, key, len, modified_ms)),
                }
            }
        }
        let parsed: Vec<(String, Entry<Option<Session>>)> = misses
            .into_par_iter()
            .map(|(path, key, len, modified_ms)| {
                (
                    key,
                    Entry {
                        len,
                        modified_ms,
                        value: parse(&path),
                    },
                )
            })
            .collect();
        if !parsed.is_empty() {
            let mut inner = self.inner.lock().unwrap();
            inner.dirty = true;
            for (key, entry) in parsed {
                sessions.extend(entry.value.clone());
                inner.store.sessions.insert(key, entry);
            }
        }
        sessions
    }

    pub fn title(&self, path: &Path, parse: impl Fn(&Path) -> Option<String>) -> Option<String> {
        let key = path.to_string_lossy().into_owned();
        let (len, modified_ms) = stamp(path)?;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.accessed.insert(key.clone());
            if let Some(entry) = inner.store.titles.get(&key)
                && entry.len == len
                && entry.modified_ms == modified_ms
            {
                return entry.value.clone();
            }
        }
        let value = parse(path);
        let mut inner = self.inner.lock().unwrap();
        inner.dirty = true;
        inner.store.titles.insert(
            key,
            Entry {
                len,
                modified_ms,
                value: value.clone(),
            },
        );
        value
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut inner = self.inner.lock().unwrap();
        let accessed = std::mem::take(&mut inner.accessed);
        let before = inner.store.sessions.len() + inner.store.titles.len();
        let live = |key: &String| accessed.contains(key) || Path::new(key).exists();
        inner.store.sessions.retain(|key, _| live(key));
        inner.store.titles.retain(|key, _| live(key));
        if inner.store.sessions.len() + inner.store.titles.len() < before {
            inner.dirty = true;
        }
        if !inner.dirty {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(&inner.store) else {
            return;
        };
        let Some(dir) = path.parent() else { return };
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, path).is_ok() {
            inner.dirty = false;
        }
    }
}

fn stamp(path: &Path) -> Option<(u64, i64)> {
    let meta = fs::metadata(path).ok()?;
    let modified: jiff::Timestamp = meta.modified().ok()?.try_into().ok()?;
    Some((meta.len(), modified.as_millisecond()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::session::Agent;

    use super::*;

    fn session(id: &str) -> Session {
        Session {
            agent: Agent::ClaudeCode,
            id: id.to_owned(),
            title: Some("t".to_owned()),
            cwd: None,
            branch: None,
            created_at: "2026-06-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-07-01T00:00:00Z".parse().unwrap(),
            path: None,
        }
    }

    #[test]
    fn default_path_uses_temporary_directory() {
        assert_eq!(
            Cache::default_path(),
            std::env::temp_dir().join("ap/sessions.json")
        );
    }

    #[test]
    fn old_schema_cache_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("sessions.json");
        fs::write(
            &store,
            r#"{"sessions":{"old":{"len":1,"modified_ms":1,"value":{"agent":"codex","id":"old","title":null,"cwd":null,"branch":null,"updated_at":"2026-07-01T00:00:00Z","path":null}}},"titles":{}}"#,
        )
        .unwrap();
        let cache = Cache::load(Some(store));
        assert!(cache.inner.lock().unwrap().store.sessions.is_empty());
    }

    #[test]
    fn hits_persist_across_loads_and_size_change_reparses() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        fs::write(&file, "one").unwrap();
        let store = dir.path().join("cache/sessions.json");
        let parses = AtomicUsize::new(0);
        let parse = |_: &Path| {
            parses.fetch_add(1, Ordering::SeqCst);
            Some(session("a"))
        };

        let cache = Cache::load(Some(store.clone()));
        assert_eq!(cache.sessions(vec![file.clone()], parse).len(), 1);
        cache.save();
        assert_eq!(parses.load(Ordering::SeqCst), 1);

        let cache = Cache::load(Some(store.clone()));
        let hits = cache.sessions(vec![file.clone()], parse);
        assert_eq!(hits[0].id, "a");
        assert_eq!(parses.load(Ordering::SeqCst), 1);

        fs::write(&file, "grown").unwrap();
        assert_eq!(cache.sessions(vec![file.clone()], parse).len(), 1);
        assert_eq!(parses.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn files_yielding_no_session_are_not_reparsed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sidechain.jsonl");
        fs::write(&file, "x").unwrap();
        let parses = AtomicUsize::new(0);
        let parse = |_: &Path| {
            parses.fetch_add(1, Ordering::SeqCst);
            None
        };
        let cache = Cache::load(Some(dir.path().join("sessions.json")));
        assert!(cache.sessions(vec![file.clone()], parse).is_empty());
        assert!(cache.sessions(vec![file], parse).is_empty());
        assert_eq!(parses.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn save_prunes_deleted_files_but_keeps_unvisited_existing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept.jsonl");
        let gone = dir.path().join("gone.jsonl");
        fs::write(&kept, "x").unwrap();
        fs::write(&gone, "x").unwrap();
        let store = dir.path().join("sessions.json");

        let cache = Cache::load(Some(store.clone()));
        cache.sessions(vec![kept.clone(), gone.clone()], |_| Some(session("a")));
        cache.save();

        fs::remove_file(&gone).unwrap();
        let cache = Cache::load(Some(store.clone()));
        cache.title(&kept, |_| Some("t".to_owned()));
        cache.save();

        let keys: HashSet<String> = Cache::load(Some(store))
            .inner
            .lock()
            .unwrap()
            .store
            .sessions
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, HashSet::from([kept.to_string_lossy().into_owned()]));
    }

    #[test]
    fn titles_cache_by_content_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("rollout.jsonl");
        fs::write(&file, "x").unwrap();
        let parses = AtomicUsize::new(0);
        let parse = |_: &Path| {
            parses.fetch_add(1, Ordering::SeqCst);
            Some("first prompt".to_owned())
        };
        let cache = Cache::load(Some(dir.path().join("sessions.json")));
        assert_eq!(cache.title(&file, parse).as_deref(), Some("first prompt"));
        assert_eq!(cache.title(&file, parse).as_deref(), Some("first prompt"));
        assert_eq!(parses.load(Ordering::SeqCst), 1);
        assert_eq!(cache.title(&dir.path().join("missing.jsonl"), parse), None);
        assert_eq!(parses.load(Ordering::SeqCst), 1);
    }
}
