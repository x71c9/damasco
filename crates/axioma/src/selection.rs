//! Publishing part of the changes. A selection names changes by key:
//! `page:<id>`, `media:<id>`, or `code` for all code and schema together.
//! The version that goes online is the published one with the chosen
//! changes taken from the last saved version; it is composed in a separate
//! git index, so the site's files and its history stay as they are.
//!
//! What a chosen change needs comes with it, with the reason: a new picture
//! a chosen page shows, a new page a chosen page sits inside or links to.
//! The composed version then passes the checks every save passes; when it
//! does not, nothing goes online and the problems say why.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::changes::{Pending, Status};

/// A change brought in by another, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Included {
    pub key: String,
    pub reason: String,
}

/// What a selection publishes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Every change that goes online: the chosen ones and those they need.
    pub keys: Vec<String>,
    pub included: Vec<Included>,
    /// Why the selection cannot go online by itself, in the panel's words.
    pub problems: Vec<String>,
    /// The selection holds every change: publishing it is publishing all.
    pub all: bool,
}

/// Every change of the list, by key, with its files.
fn keyed(pending: &Pending) -> BTreeMap<String, Vec<(String, Status)>> {
    let mut keys = BTreeMap::new();
    for item in &pending.pages {
        keys.insert(
            format!("page:{}", item.id),
            vec![(format!("content/pages/{}.json", item.id), item.status)],
        );
    }
    for item in &pending.media {
        keys.insert(
            format!("media:{}", item.id),
            vec![(format!("content/media/{}.json", item.id), item.status)],
        );
    }
    if !pending.files.is_empty() {
        keys.insert(
            "code".to_string(),
            pending
                .files
                .iter()
                .map(|file| (file.path.clone(), file.status))
                .collect(),
        );
    }
    keys
}

/// The pages and pictures a page refers to: its parent, `{"page": id}`
/// (links and page fields) and `{"media": id}` anywhere in it.
fn references(page: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
    fn walk(value: &Value, pages: &mut BTreeSet<String>, media: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(id) = object.get("page").and_then(Value::as_str) {
                    pages.insert(id.to_string());
                }
                if let Some(id) = object.get("media").and_then(Value::as_str) {
                    media.insert(id.to_string());
                }
                object.values().for_each(|child| walk(child, pages, media));
            }
            Value::Array(items) => items.iter().for_each(|child| walk(child, pages, media)),
            _ => {}
        }
    }
    let mut pages = BTreeSet::new();
    let mut media = BTreeSet::new();
    if let Some(parent) = page["inside"].as_str() {
        pages.insert(parent.to_string());
    }
    for key in ["fields", "blocks", "seo"] {
        walk(&page[key], &mut pages, &mut media);
    }
    (pages, media)
}

/// The changes `chosen` needs, added with their reasons, and whether that
/// is every change.
pub fn plan(site: &Path, pending: &Pending, chosen: &[String]) -> Result<Plan, String> {
    let keys = keyed(pending);
    let mut set: BTreeSet<String> = BTreeSet::new();
    for key in chosen {
        if !keys.contains_key(key) {
            return Err(format!("{key} is not among the changes to publish"));
        }
        set.insert(key.clone());
    }
    if set.is_empty() {
        return Err("choose at least one change".into());
    }
    let titles: HashMap<String, String> = pending
        .pages
        .iter()
        .map(|item| (format!("page:{}", item.id), item.title.clone()))
        .collect();
    let new: BTreeSet<String> = pending
        .pages
        .iter()
        .map(|item| (format!("page:{}", item.id), item.status))
        .chain(
            pending
                .media
                .iter()
                .map(|item| (format!("media:{}", item.id), item.status)),
        )
        .filter(|(_, status)| *status == Status::Created)
        .map(|(key, _)| key)
        .collect();

    // Pages read once, as saved now.
    let page_keys: Vec<String> = pending
        .pages
        .iter()
        .filter(|item| item.status != Status::Deleted)
        .map(|item| format!("page:{}", item.id))
        .collect();
    let specs: Vec<String> = page_keys
        .iter()
        .map(|key| format!("HEAD:content/pages/{}.json", &key[5..]))
        .collect();
    let pages: HashMap<String, Value> = page_keys
        .into_iter()
        .zip(crate::git::show_many(site, &specs)?)
        .filter_map(|(key, bytes)| Some((key, serde_json::from_slice(&bytes?).ok()?)))
        .collect();

    let mut included = Vec::new();
    let mut queue: Vec<String> = set.iter().cloned().collect();
    while let Some(key) = queue.pop() {
        let Some(page) = pages.get(&key) else {
            continue;
        };
        let title = titles.get(&key).cloned().unwrap_or_default();
        let (linked, shown) = references(page);
        let needs = linked
            .into_iter()
            .map(|id| {
                let parent = page["inside"].as_str() == Some(id.as_str());
                (
                    format!("page:{id}"),
                    if parent {
                        format!("\u{201c}{title}\u{201d} is inside it")
                    } else {
                        format!("\u{201c}{title}\u{201d} links to it")
                    },
                )
            })
            .chain(shown.into_iter().map(|id| {
                (
                    format!("media:{id}"),
                    format!("\u{201c}{title}\u{201d} uses it"),
                )
            }));
        for (need, why) in needs {
            if new.contains(&need) && set.insert(need.clone()) {
                included.push(Included {
                    key: need.clone(),
                    reason: format!("Included because {why}"),
                });
                queue.push(need);
            }
        }
    }

    let all = set.len() == keys.len();
    Ok(Plan {
        keys: set.into_iter().collect(),
        included,
        problems: Vec::new(),
        all,
    })
}

/// The scratch index the selections are composed in.
fn index_of(site: &Path) -> PathBuf {
    site.join(".git").join("axioma-selection.index")
}

/// Composes the published version with the changes of `keys` from the last
/// saved one, as a commit on top of the published version. Returns it.
pub fn compose(
    site: &Path,
    pending: &Pending,
    keys: &[String],
    person: i64,
) -> Result<String, String> {
    let index = index_of(site);
    let _ = std::fs::remove_file(&index);
    let files = keyed(pending);
    match &pending.published {
        Some(published) => {
            crate::git::run_with(site, &["read-tree", published], Some(&index), None)?
        }
        None => crate::git::run_with(site, &["read-tree", "--empty"], Some(&index), None)?,
    };
    // One line per file for update-index: the file as saved now, or its
    // removal.
    let taken: Vec<&(String, Status)> = keys
        .iter()
        .filter_map(|key| files.get(key))
        .flatten()
        .collect();
    let present: Vec<&str> = taken
        .iter()
        .filter(|(_, status)| *status != Status::Deleted)
        .map(|(path, _)| path.as_str())
        .collect();
    let mut lines = String::new();
    if !present.is_empty() {
        let mut args = vec!["ls-tree", "-z", "HEAD", "--"];
        args.extend(present.iter().copied());
        let listed = crate::git::run(site, &args)?;
        for entry in listed.split('\0').filter(|entry| !entry.is_empty()) {
            // "<mode> blob <object>\t<path>"
            let (meta, path) = entry
                .split_once('\t')
                .ok_or("git ls-tree: unexpected output")?;
            let mut parts = meta.split(' ');
            let (Some(mode), Some(_), Some(object)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err("git ls-tree: unexpected output".into());
            };
            lines.push_str(&format!("{mode} {object}\t{path}\0"));
        }
    }
    for (path, status) in &taken {
        if *status == Status::Deleted {
            lines.push_str(&format!(
                "0 0000000000000000000000000000000000000000\t{path}\0"
            ));
        }
    }
    crate::git::run_with(
        site,
        &["update-index", "-z", "--index-info"],
        Some(&index),
        Some(&lines),
    )?;
    let tree = crate::git::run_with(site, &["write-tree"], Some(&index), None)?;
    let message = format!("Published a selection of the changes\n\nIn the panel: person {person}.");
    let mut args = vec!["commit-tree", tree.trim(), "-m", &message];
    if let Some(published) = &pending.published {
        args.extend(["-p", published]);
    }
    let commit = crate::git::run(site, &args)?.trim().to_string();
    // Kept by a ref, so that git never collects it.
    crate::git::run(site, &["update-ref", "refs/axioma/selection", &commit])?;
    Ok(commit)
}

/// The problems of a composed version: the checks every save passes, run
/// on its files, written out in `dir`. Pages are named by their titles.
pub fn check(
    site: &Path,
    commit: &str,
    dir: &Path,
    titles: &HashMap<String, String>,
) -> Result<Vec<String>, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let index = index_of(site);
    crate::git::run_with(site, &["read-tree", commit], Some(&index), None)?;
    let prefix = format!("{}/", dir.display());
    crate::git::run_with(
        site,
        &["checkout-index", "-a", "-f", &format!("--prefix={prefix}")],
        Some(&index),
        None,
    )?;
    let name = |at: &str| {
        let mut text = at.to_string();
        for (id, title) in titles {
            text = text.replace(
                &format!("content/pages/{id}.json"),
                &format!("\u{201c}{title}\u{201d}"),
            );
        }
        text
    };
    let problems = match crate::schema::Schema::load(dir) {
        Err(found) => found,
        Ok(schema) => schema.check_site(dir),
    };
    let _ = std::fs::remove_dir_all(dir);
    Ok(problems
        .iter()
        .map(|problem| format!("{}: {}", name(&problem.at), problem.message))
        .collect())
}

/// A selection ready to build: its plan and, when it can go online by
/// itself, the composed version. Selecting everything composes nothing:
/// that is the version the preview already shows.
pub struct Prepared {
    pub plan: Plan,
    pub commit: Option<String>,
}

pub fn prepare(
    site: &Path,
    pending: &Pending,
    chosen: &[String],
    person: i64,
    check_dir: &Path,
) -> Result<Prepared, String> {
    let mut plan = plan(site, pending, chosen)?;
    if plan.all {
        return Ok(Prepared { plan, commit: None });
    }
    let commit = compose(site, pending, &plan.keys, person)?;
    let titles: HashMap<String, String> = pending
        .pages
        .iter()
        .map(|item| (item.id.clone(), item.title.clone()))
        .collect();
    plan.problems = check(site, &commit, check_dir, &titles)?;
    Ok(Prepared {
        commit: plan.problems.is_empty().then_some(commit),
        plan,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::{mark_published, pending};
    use crate::git::{commit_all, head, run};
    use serde_json::json;

    fn page(site: &Path, id: &str, title: &str, inside: Option<&str>, blocks: Value) {
        let value = json!({
            "id": id, "type": "page", "lang": "it", "inside": inside, "order": 0,
            "slug": if inside.is_none() { String::new() } else { title.to_lowercase().replace(' ', "-") },
            "title": title, "menu": true, "fields": {}, "blocks": blocks,
            "seo": { "title": null, "description": null, "image": null, "hidden": false }
        });
        std::fs::write(
            site.join(format!("content/pages/{id}.json")),
            value.to_string(),
        )
        .unwrap();
    }

    const HOME: &str = "01HZZZZZZZZZZZZZZZZZZZZZZA";
    const LAKE: &str = "01HZZZZZZZZZZZZZZZZZZZZZZB";
    const NEWS: &str = "01HZZZZZZZZZZZZZZZZZZZZZZC";
    const PIC: &str = "01HZZZZZZZZZZZZZZZZZZZZZZD";

    #[test]
    fn a_selection_brings_what_it_needs_and_leaves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        for sub in ["content/pages", "content/media", "schema/types", "src"] {
            std::fs::create_dir_all(site.join(sub)).unwrap();
        }
        std::fs::write(
            site.join("schema/types/page.json"),
            r#"{ "name": "page", "label": "Page", "fields": [] }"#,
        )
        .unwrap();
        std::fs::write(
            site.join("axioma.json"),
            r#"{ "name": "Studio", "lang": "it" }"#,
        )
        .unwrap();
        std::fs::write(site.join("src/layout.astro"), "one").unwrap();
        run(&site, &["init", "-q", "-b", "master"]).unwrap();
        page(&site, HOME, "Home", None, json!([]));
        commit_all(&site, "chore: new site").unwrap();
        mark_published(&site, &head(&site).unwrap()).unwrap();

        // A new picture, a new page showing it, a new page inside that one,
        // and a change of the code.
        std::fs::write(
            site.join(format!("content/media/{PIC}.json")),
            json!({ "id": PIC, "name": "lake.png" }).to_string(),
        )
        .unwrap();
        page(
            &site,
            LAKE,
            "Lake",
            Some(HOME),
            json!([{ "id": "b1", "type": "media", "media": { "media": PIC }, "caption": null }]),
        );
        page(&site, NEWS, "News", Some(LAKE), json!([]));
        std::fs::write(site.join("src/layout.astro"), "two").unwrap();
        commit_all(&site, "Changes\n\nIn the panel: person 1.").unwrap();
        let list = pending(&site).unwrap();
        assert_eq!(list.count(), 4);

        // News alone needs Lake, and Lake its picture.
        let chosen = plan(&site, &list, &[format!("page:{NEWS}")]).unwrap();
        assert_eq!(
            chosen.keys,
            [
                format!("media:{PIC}"),
                format!("page:{LAKE}"),
                format!("page:{NEWS}")
            ]
        );
        assert_eq!(chosen.included.len(), 2);
        assert!(
            chosen
                .included
                .iter()
                .any(|i| i.reason == "Included because \u{201c}News\u{201d} is inside it")
        );
        assert!(
            chosen
                .included
                .iter()
                .any(|i| i.reason == "Included because \u{201c}Lake\u{201d} uses it")
        );
        assert!(!chosen.all);
        assert!(plan(&site, &list, &["page:nothing".into()]).is_err());

        // Composed: the pages and the picture, not the code.
        let commit = compose(&site, &list, &chosen.keys, 1).unwrap();
        let files = run(&site, &["ls-tree", "-r", "--name-only", &commit]).unwrap();
        assert!(files.contains(&format!("content/pages/{NEWS}.json")));
        assert!(files.contains(&format!("content/media/{PIC}.json")));
        assert_eq!(
            run(&site, &["show", &format!("{commit}:src/layout.astro")]).unwrap(),
            "one"
        );
        let titles = HashMap::new();
        assert_eq!(
            check(&site, &commit, &dir.path().join("check"), &titles).unwrap(),
            Vec::<String>::new()
        );
        // The site itself did not move.
        assert_eq!(
            std::fs::read_to_string(site.join("src/layout.astro")).unwrap(),
            "two"
        );

        // A page without the picture it shows does not pass the checks.
        let without = compose(&site, &list, &[format!("page:{LAKE}")], 1).unwrap();
        let titles: HashMap<String, String> = [(LAKE.to_string(), "Lake".to_string())].into();
        let problems = check(&site, &without, &dir.path().join("check"), &titles).unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].starts_with("\u{201c}Lake\u{201d}"),
            "{problems:?}"
        );

        // Published: what was left out stays pending, alone.
        mark_published(&site, &commit).unwrap();
        let left = pending(&site).unwrap();
        assert_eq!(left.count(), 1);
        assert_eq!(left.files.len(), 1);
    }
}
