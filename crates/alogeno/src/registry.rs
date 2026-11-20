//! The sites at work: one `SiteRuntime` per site opened since the start,
//! with its database, its builds and its chat, created on first use. A
//! published site needs none of them: its files are served from the disk.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::build::Builder;
use crate::chat::Chat;
use crate::config::Config;
use crate::db;
use crate::resolver;
use crate::sites::{self, Site};

/// One site at work.
pub struct SiteRuntime {
    /// `data_dir/sites/<id>`.
    pub root: PathBuf,
    /// The site's own database.
    pub db: db::Handle,
    pub builder: Arc<Builder>,
    pub chat: Arc<Chat>,
}

/// Starts the runtime of a site: opens its database, its builds, its chat.
pub type Factory = Arc<
    dyn Fn(Site) -> Pin<Box<dyn Future<Output = Result<Arc<SiteRuntime>, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, PartialEq, Eq)]
pub enum SiteError {
    NotFound,
    /// The site exists but cannot start, for example its database.
    Unavailable(String),
}

pub struct Registry {
    /// The installation's database.
    db: db::Handle,
    config: Config,
    resolver: resolver::Shared,
    factory: Factory,
    loaded: Mutex<HashMap<String, Arc<SiteRuntime>>>,
    /// One start at a time, so that two first requests to a site do not
    /// start it twice.
    starting: tokio::sync::Mutex<()>,
}

impl Registry {
    pub fn new(
        db: db::Handle,
        config: Config,
        resolver: resolver::Shared,
        factory: Factory,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            config,
            resolver,
            factory,
            loaded: Mutex::new(HashMap::new()),
            starting: tokio::sync::Mutex::new(()),
        })
    }

    fn loaded_map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<SiteRuntime>>> {
        self.loaded.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every site, oldest first.
    pub async fn list(&self) -> Result<Vec<Site>, String> {
        self.db
            .run(sites::list)
            .await
            .map_err(|err| err.to_string())
    }

    /// The runtime of the site `id`, started if it is not yet.
    pub async fn get(&self, id: &str) -> Result<Arc<SiteRuntime>, SiteError> {
        if let Some(runtime) = self.loaded(id) {
            return Ok(runtime);
        }
        let _one_at_a_time = self.starting.lock().await;
        if let Some(runtime) = self.loaded(id) {
            return Ok(runtime);
        }
        let site = self
            .list()
            .await
            .map_err(SiteError::Unavailable)?
            .into_iter()
            .find(|site| site.id == id)
            .ok_or(SiteError::NotFound)?;
        let runtime = (self.factory)(site).await.map_err(SiteError::Unavailable)?;
        self.loaded_map().insert(id.to_string(), runtime.clone());
        Ok(runtime)
    }

    /// The runtime of the site `id` if it is started; never starts one.
    pub fn loaded(&self, id: &str) -> Option<Arc<SiteRuntime>> {
        self.loaded_map().get(id).cloned()
    }

    /// Every runtime started so far.
    pub fn all_loaded(&self) -> Vec<Arc<SiteRuntime>> {
        self.loaded_map().values().cloned().collect()
    }

    /// Creates a site and makes its addresses known.
    pub async fn create(&self, name: String, slug: String) -> Result<Site, String> {
        let data_dir = self.config.data_dir.clone();
        let site = self
            .db
            .run(move |conn| sites::create(conn, &data_dir, &name, &slug))
            .await?;
        self.refresh().await?;
        Ok(site)
    }

    /// Rebuilds the table of addresses from the sites.
    pub async fn refresh(&self) -> Result<(), String> {
        let list = self.list().await?;
        resolver::rebuild(&self.resolver, &self.config, &list);
        Ok(())
    }
}

/// For tests of the HTTP layer: a registry whose one site, `01TESTSITE`
/// (slug `main`), runs with `site_db`, idle builds and an idle chat, over
/// the installation database `installation`.
#[cfg(test)]
pub fn single(
    installation: rusqlite::Connection,
    site_db: db::Handle,
    config: &Config,
) -> (db::Handle, Arc<Registry>, resolver::Shared) {
    installation
        .execute(
            "INSERT INTO site (id, slug, name) VALUES ('01TESTSITE', 'main', 'Main')",
            [],
        )
        .expect("test site");
    let db = db::Handle::new(installation);
    let runtime = Arc::new(SiteRuntime {
        root: sites::root(&config.data_dir, "01TESTSITE"),
        db: site_db.clone(),
        builder: crate::build::idle(&config.data_dir),
        chat: crate::chat::idle(db.clone(), site_db),
    });
    let site = Site {
        id: "01TESTSITE".into(),
        slug: "main".into(),
        name: "Main".into(),
    };
    let resolver = resolver::Resolver::shared(config, std::slice::from_ref(&site));
    let factory: Factory = Arc::new(move |_| {
        let runtime = runtime.clone();
        Box::pin(async move { Ok(runtime) })
    });
    let registry = Registry::new(db.clone(), config.clone(), resolver.clone(), factory);
    (db, registry, resolver)
}
