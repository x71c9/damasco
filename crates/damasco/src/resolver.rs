//! Which part of the installation a hostname reaches: the panel, the
//! preview of a site, a published site, or a redirect to one of its
//! domains. The table is built from the configuration, the sites and their
//! domains, and rebuilt whole after every change. The HTTP dispatch reads
//! it, and so does the reverse proxy's question of whether a domain is
//! served here (`ask`), so that both always agree.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::config::Config;
use crate::domains::Domain;
use crate::sites::Site;

/// What a hostname reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Surface {
    Panel,
    /// The preview of the site with this id.
    Preview(String),
    /// The published site with this id.
    Live(String),
    /// A domain that sends its visitors to this one of the same site: the
    /// www added with a domain.
    Redirect(String),
}

#[derive(Debug, Default)]
pub struct Resolver {
    hosts: HashMap<String, Surface>,
}

/// The table in use, replaced whole by `rebuild`.
pub type Shared = Arc<RwLock<Arc<Resolver>>>;

impl Resolver {
    /// The table for `config`, `sites` and their `domains`.
    pub fn build(config: &Config, sites: &[Site], domains: &[Domain]) -> Self {
        let mut hosts = HashMap::new();
        hosts.insert(config.hosts.panel.clone(), Surface::Panel);
        for site in sites {
            hosts.insert(
                config.preview_host(&site.slug),
                Surface::Preview(site.id.clone()),
            );
            if let Some(host) = config.local_host(&site.slug) {
                hosts.insert(host, Surface::Live(site.id.clone()));
            }
        }
        // Adding a domain refuses the names above; were one there anyway,
        // the panel and the previews keep it.
        for domain in domains {
            let surface = match &domain.redirect_to {
                Some(target) => Surface::Redirect(target.clone()),
                None => Surface::Live(domain.site_id.clone()),
            };
            hosts.entry(domain.host.clone()).or_insert(surface);
        }
        Self { hosts }
    }

    /// What `host` reaches, if anything; any case, with or without the
    /// trailing dot.
    pub fn resolve(&self, host: &str) -> Option<Surface> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.hosts.get(&host).cloned()
    }

    /// The table shared by the server.
    pub fn shared(config: &Config, sites: &[Site], domains: &[Domain]) -> Shared {
        Arc::new(RwLock::new(Arc::new(Self::build(config, sites, domains))))
    }
}

/// Replaces the table in use.
pub fn rebuild(shared: &Shared, config: &Config, sites: &[Site], domains: &[Domain]) {
    let table = Arc::new(Resolver::build(config, sites, domains));
    *shared.write().unwrap_or_else(|e| e.into_inner()) = table;
}

/// The table in use, for one lookup or several.
pub fn current(shared: &Shared) -> Arc<Resolver> {
    shared.read().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(id: &str, slug: &str) -> Site {
        Site {
            id: id.into(),
            slug: slug.into(),
            name: slug.into(),
        }
    }

    fn config(panel: &str) -> Config {
        Config::parse(&format!(
            "listen = \"127.0.0.1:4100\"\ndata_dir = \"/d\"\n[hosts]\npanel = \"{panel}\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn each_host_reaches_its_surface() {
        let sites = [site("A", "blog"), site("B", "shop")];
        let table = Resolver::build(&config("admin.example.com"), &sites, &[]);
        assert_eq!(table.resolve("admin.example.com"), Some(Surface::Panel));
        assert_eq!(
            table.resolve("Blog.Preview.Admin.Example.com."),
            Some(Surface::Preview("A".into()))
        );
        assert_eq!(
            table.resolve("shop.preview.admin.example.com"),
            Some(Surface::Preview("B".into()))
        );
        for unknown in [
            "other.preview.admin.example.com",
            "preview.admin.example.com",
            "blog.admin.example.com",
            "blog.example.com",
            "example.com",
        ] {
            assert_eq!(table.resolve(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn locally_each_site_is_also_published_next_to_the_panel() {
        let sites = [site("A", "blog")];
        let table = Resolver::build(&config("admin.localhost"), &sites, &[]);
        assert_eq!(
            table.resolve("blog.localhost"),
            Some(Surface::Live("A".into()))
        );
        assert_eq!(
            table.resolve("blog.preview.admin.localhost"),
            Some(Surface::Preview("A".into()))
        );
    }

    #[test]
    fn a_rebuild_drops_a_removed_site() {
        let config = config("admin.example.com");
        let shared = Resolver::shared(&config, &[site("A", "blog")], &[]);
        assert!(
            current(&shared)
                .resolve("blog.preview.admin.example.com")
                .is_some()
        );
        rebuild(&shared, &config, &[], &[]);
        assert_eq!(
            current(&shared).resolve("blog.preview.admin.example.com"),
            None
        );
    }

    #[test]
    fn a_domain_publishes_its_site_and_its_www_redirects() {
        let domains = [
            Domain {
                host: "shop.it".into(),
                site_id: "B".into(),
                redirect_to: None,
            },
            Domain {
                host: "www.shop.it".into(),
                site_id: "B".into(),
                redirect_to: Some("shop.it".into()),
            },
        ];
        let sites = [site("A", "blog"), site("B", "shop")];
        let table = Resolver::build(&config("admin.example.com"), &sites, &domains);
        assert_eq!(table.resolve("Shop.it."), Some(Surface::Live("B".into())));
        assert_eq!(
            table.resolve("www.shop.it"),
            Some(Surface::Redirect("shop.it".into()))
        );
        assert_eq!(table.resolve("blog.it"), None);
    }
}
