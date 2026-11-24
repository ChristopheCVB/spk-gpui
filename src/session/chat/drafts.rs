use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use tokio::task::spawn_blocking;

type Bodies = HashMap<String, String>;
type StoredDrafts = HashMap<String, HashMap<String, Bodies>>;

#[derive(Clone)]
pub(in crate::session) struct DraftStore {
    path: PathBuf,
    origin: String,
    account: String,
}

impl DraftStore {
    pub(in crate::session) fn new(path: PathBuf, origin: String, account: String) -> Self {
        Self {
            path,
            origin,
            account,
        }
    }

    pub async fn load(&self) -> io::Result<Bodies> {
        let store = self.clone();
        spawn_blocking(move || {
            let mut drafts = read(&store.path)?;
            Ok(drafts
                .get_mut(&store.origin)
                .and_then(|accounts| accounts.remove(&store.account))
                .unwrap_or_default())
        })
        .await?
    }

    pub async fn save(&self, changes: Bodies) -> io::Result<()> {
        let store = self.clone();
        spawn_blocking(move || {
            let directory = store.path.parent().expect("draft file has a parent");
            fs::create_dir_all(directory)?;
            // Lock a separate file so replacing the Draft file keeps the lock on the same inode.
            let lock = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(store.path.with_extension("lock"))?;
            lock.lock()?;

            let mut drafts = read(&store.path)?;
            let bodies = drafts
                .entry(store.origin)
                .or_default()
                .entry(store.account)
                .or_default();
            for (channel, body) in changes {
                if body.is_empty() {
                    bodies.remove(&channel);
                } else {
                    bodies.insert(channel, body);
                }
            }
            let mut file = tempfile::NamedTempFile::new_in(directory)?;
            serde_json::to_writer(&mut file, &drafts)?;
            file.as_file().sync_all()?;
            file.persist(store.path)?;
            Ok(())
        })
        .await?
    }
}

fn read(path: &Path) -> io::Result<StoredDrafts> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(error),
    }
}
