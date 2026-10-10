//! The content of the site: its pages, read and changed only through here.
//!
//! Every change is checked against the schema and the page tree before it
//! is written, and every page is written in its canonical form, through a
//! temporary file and a rename, so a page on disk is never half written.
//! The assistant reaches these operations through axioma's MCP tools; the
//! panel's editor will use the same ones.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::schema::{self, Problem, Schema};

/// A page as listed: enough to find it and place it in the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub id: String,
    #[serde(rename = "type")]
    pub page_type: String,
    pub title: String,
    pub slug: String,
    pub inside: Option<String>,
    pub order: i64,
    pub menu: bool,
    /// The public address, from the slugs of the page and its parents.
    pub address: String,
}

/// Why an operation did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum CmsError {
    NotFound,
    /// The change would leave the site invalid.
    Invalid(Vec<Problem>),
    /// The operation is not possible, with the reason.
    Refused(String),
    Io(String),
}

impl std::fmt::Display for CmsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmsError::NotFound => write!(f, "no page has this id"),
            CmsError::Invalid(problems) => {
                write!(f, "the page was not saved:")?;
                for problem in problems {
                    write!(f, "\n- {problem}")?;
                }
                Ok(())
            }
            CmsError::Refused(reason) => write!(f, "{reason}"),
            CmsError::Io(problem) => write!(f, "{problem}"),
        }
    }
}

/// Keys a change may set. `id` and `lang` never change.
const CHANGEABLE: [&str; 9] = [
    "type", "inside", "order", "slug", "title", "menu", "fields", "blocks", "seo",
];

pub struct Cms {
    site: PathBuf,
}

impl Cms {
    pub fn new(site: &Path) -> Self {
        Self {
            site: site.to_path_buf(),
        }
    }

    fn pages_dir(&self) -> PathBuf {
        self.site.join("content/pages")
    }

    fn schema(&self) -> Result<Schema, CmsError> {
        Schema::load(&self.site).map_err(CmsError::Invalid)
    }

    /// Every page, as stored.
    fn all(&self) -> Result<Vec<Value>, CmsError> {
        let mut pages = Vec::new();
        let entries = match std::fs::read_dir(self.pages_dir()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(pages),
            Err(err) => return Err(CmsError::Io(err.to_string())),
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let text = std::fs::read_to_string(&path).map_err(|e| CmsError::Io(e.to_string()))?;
            let page = serde_json::from_str(&text)
                .map_err(|e| CmsError::Io(format!("{}: {e}", path.display())))?;
            pages.push(page);
        }
        Ok(pages)
    }

    /// The pages in tree order: the home, then each page followed by the
    /// pages inside it.
    pub fn list(&self) -> Result<Vec<Summary>, CmsError> {
        let pages = self.all()?;
        let text = |page: &Value, key: &str| page[key].as_str().unwrap_or_default().to_string();
        let mut summaries: Vec<Summary> = pages
            .iter()
            .map(|page| Summary {
                id: text(page, "id"),
                page_type: text(page, "type"),
                title: text(page, "title"),
                slug: text(page, "slug"),
                inside: page["inside"].as_str().map(String::from),
                order: page["order"].as_i64().unwrap_or_default(),
                menu: page["menu"].as_bool().unwrap_or_default(),
                address: String::new(),
            })
            .collect();
        let addresses: Vec<String> = summaries
            .iter()
            .map(|summary| address_of(&summaries, summary))
            .collect();
        for (summary, address) in summaries.iter_mut().zip(addresses) {
            summary.address = address;
        }
        let mut ordered = Vec::new();
        place(&summaries, None, &mut ordered, 0);
        Ok(ordered)
    }

    pub fn read(&self, id: &str) -> Result<Value, CmsError> {
        if !schema::is_ulid(id) {
            return Err(CmsError::NotFound);
        }
        let text = std::fs::read_to_string(self.pages_dir().join(format!("{id}.json")))
            .map_err(|_| CmsError::NotFound)?;
        serde_json::from_str(&text).map_err(|e| CmsError::Io(e.to_string()))
    }

    /// The page types and their fields.
    pub fn types(&self) -> Result<Value, CmsError> {
        let schema = self.schema()?;
        Ok(json!(schema.types.values().collect::<Vec<_>>()))
    }

    /// Applies `changes` to a page: each key given replaces the page's
    /// value, except `fields` and `seo`, whose keys are merged.
    pub fn update(&self, id: &str, changes: &Value) -> Result<Value, CmsError> {
        let mut page = self.read(id)?;
        let changes = changes
            .as_object()
            .ok_or_else(|| CmsError::Refused("the changes must be an object".into()))?;
        for (key, value) in changes {
            if !CHANGEABLE.contains(&key.as_str()) {
                return Err(CmsError::Refused(format!(
                    "{key:?} cannot be changed; the keys are {}",
                    CHANGEABLE.join(", ")
                )));
            }
            match (key.as_str(), value, page.get_mut(key)) {
                ("fields" | "seo", Value::Object(new), Some(Value::Object(old))) => {
                    for (name, value) in new {
                        old.insert(name.clone(), value.clone());
                    }
                }
                _ => {
                    page[key] = value.clone();
                }
            }
        }
        self.save(page)
    }

    /// Creates a page. `inside` and `title` are needed; the address comes
    /// from the title when `slug` is missing, and the page goes after its
    /// siblings when `order` is missing.
    pub fn create(&self, fields: &Value) -> Result<Value, CmsError> {
        let given = fields
            .as_object()
            .ok_or_else(|| CmsError::Refused("the page must be an object".into()))?;
        for key in given.keys() {
            if !CHANGEABLE.contains(&key.as_str()) {
                return Err(CmsError::Refused(format!(
                    "{key:?} cannot be set; the keys are {}",
                    CHANGEABLE.join(", ")
                )));
            }
        }
        let inside = given.get("inside").and_then(Value::as_str).ok_or_else(|| {
            CmsError::Refused("a new page goes inside another: give `inside`".into())
        })?;
        let pages = self.all()?;
        let siblings = pages
            .iter()
            .filter(|p| p["inside"].as_str() == Some(inside));
        let order = siblings
            .filter_map(|p| p["order"].as_i64())
            .max()
            .map_or(0, |last| last + 1);
        let title = given
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lang = std::fs::read_to_string(self.site.join("axioma.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|settings| settings["lang"].as_str().map(String::from))
            .unwrap_or_else(|| "en".into());
        let mut page = Map::new();
        page.insert("id".into(), json!(crate::site::ulid()));
        page.insert("type".into(), json!("page"));
        page.insert("lang".into(), json!(lang));
        page.insert("inside".into(), json!(inside));
        page.insert("order".into(), json!(order));
        page.insert("slug".into(), json!(slug_of(title)));
        page.insert("title".into(), json!(title));
        page.insert("menu".into(), json!(false));
        page.insert("fields".into(), json!({}));
        page.insert("blocks".into(), json!([]));
        page.insert(
            "seo".into(),
            json!({ "title": null, "description": null, "image": null, "hidden": false }),
        );
        for (key, value) in given {
            page.insert(key.clone(), value.clone());
        }
        self.save(Value::Object(page))
    }

    /// Checks a page with the whole tree and writes it.
    fn save(&self, page: Value) -> Result<Value, CmsError> {
        let schema = self.schema()?;
        let id = page["id"].as_str().unwrap_or_default().to_string();
        let at = format!("content/pages/{id}.json");
        let mut problems = Vec::new();
        schema.check_page(&at, &page, &mut problems);
        let mut tree: Vec<(String, Value)> = self
            .all()?
            .into_iter()
            .filter(|p| p["id"].as_str() != Some(&id))
            .map(|p| {
                let at = format!(
                    "content/pages/{}.json",
                    p["id"].as_str().unwrap_or_default()
                );
                (at, p)
            })
            .collect();
        tree.push((at, page.clone()));
        schema::check_tree(&tree, &mut problems);
        if !problems.is_empty() {
            return Err(CmsError::Invalid(problems));
        }
        let text = schema::canonical(&schema, &page);
        let dir = self.pages_dir();
        std::fs::create_dir_all(&dir).map_err(|e| CmsError::Io(e.to_string()))?;
        let temporary = dir.join(format!(".{id}.json.new"));
        std::fs::write(&temporary, &text).map_err(|e| CmsError::Io(e.to_string()))?;
        std::fs::rename(&temporary, dir.join(format!("{id}.json")))
            .map_err(|e| CmsError::Io(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| CmsError::Io(e.to_string()))
    }

    /// What deleting a page would break, or nothing when it can go: the
    /// home, pages with pages inside and pages other pages link to stay.
    pub fn check_delete(&self, id: &str) -> Result<Value, CmsError> {
        let page = self.read(id)?;
        if page["inside"].is_null() {
            return Err(CmsError::Refused("the home page cannot be deleted".into()));
        }
        let pages = self.all()?;
        let title = |p: &Value| p["title"].as_str().unwrap_or_default().to_string();
        let children: Vec<String> = pages
            .iter()
            .filter(|p| p["inside"].as_str() == Some(id))
            .map(title)
            .collect();
        if !children.is_empty() {
            return Err(CmsError::Refused(format!(
                "other pages are inside it: {}; move or delete them first",
                children.join(", ")
            )));
        }
        let linking: Vec<String> = pages
            .iter()
            .filter(|p| p["id"].as_str() != Some(id) && refers_to(p, id))
            .map(title)
            .collect();
        if !linking.is_empty() {
            return Err(CmsError::Refused(format!(
                "other pages link to it: {}; change those links first",
                linking.join(", ")
            )));
        }
        Ok(page)
    }

    pub fn delete(&self, id: &str) -> Result<(), CmsError> {
        self.check_delete(id)?;
        std::fs::remove_file(self.pages_dir().join(format!("{id}.json")))
            .map_err(|e| CmsError::Io(e.to_string()))
    }
}

/// The address of a page: the slugs from the home down to it.
fn address_of(pages: &[Summary], page: &Summary) -> String {
    let mut slugs = Vec::new();
    let mut current = Some(page);
    // More steps than pages means a loop: stop there.
    for _ in 0..=pages.len() {
        let Some(here) = current else { break };
        if !here.slug.is_empty() {
            slugs.push(here.slug.as_str());
        }
        current = here
            .inside
            .as_deref()
            .and_then(|parent| pages.iter().find(|p| p.id == parent));
    }
    slugs.reverse();
    format!("/{}", slugs.join("/"))
}

/// Appends the pages inside `parent`, by order, each followed by its own.
fn place(pages: &[Summary], parent: Option<&str>, out: &mut Vec<Summary>, depth: usize) {
    if depth > pages.len() {
        return;
    }
    let mut children: Vec<&Summary> = pages
        .iter()
        .filter(|p| p.inside.as_deref() == parent)
        .collect();
    children.sort_by_key(|p| (p.order, p.title.clone()));
    for child in children {
        out.push(child.clone());
        place(pages, Some(&child.id), out, depth + 1);
    }
}

/// Whether `value` holds a reference to the page `id` anywhere.
fn refers_to(value: &Value, id: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.get("page").and_then(Value::as_str) == Some(id)
                || object.values().any(|v| refers_to(v, id))
        }
        Value::Array(items) => items.iter().any(|v| refers_to(v, id)),
        _ => false,
    }
}

/// An address made from a title: lowercase letters and digits, words joined
/// by single dashes, accents removed for the common Latin letters.
pub fn slug_of(title: &str) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        let plain = match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => "a",
            'è' | 'é' | 'ê' | 'ë' => "e",
            'ì' | 'í' | 'î' | 'ï' => "i",
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => "o",
            'ù' | 'ú' | 'û' | 'ü' => "u",
            'ç' => "c",
            'ñ' => "n",
            'ß' => "ss",
            c if c.is_ascii_alphanumeric() => {
                slug.push(c);
                continue;
            }
            _ => "-",
        };
        if plain == "-" {
            if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        } else {
            slug.push_str(plain);
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() { "page".into() } else { slug }
}

/// A site with a home page, for tests: its directory, its path and the
/// id of the home.
#[cfg(test)]
pub fn test_site() -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("site");
    std::fs::create_dir_all(path.join("content/pages")).unwrap();
    std::fs::create_dir_all(path.join("schema/types")).unwrap();
    std::fs::write(
        path.join("schema/types/page.json"),
        r#"{ "name": "page", "label": "Page", "fields": [] }"#,
    )
    .unwrap();
    std::fs::write(
        path.join("axioma.json"),
        r#"{ "name": "Studio", "lang": "it" }"#,
    )
    .unwrap();
    let home = crate::site::ulid();
    let page = json!({
        "id": home, "type": "page", "lang": "it", "inside": null, "order": 0, "slug": "",
        "title": "Studio", "menu": true, "fields": {}, "blocks": [],
        "seo": { "title": null, "description": null, "image": null, "hidden": false }
    });
    std::fs::write(
        path.join(format!("content/pages/{home}.json")),
        page.to_string(),
    )
    .unwrap();
    (dir, path, home)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> (tempfile::TempDir, Cms, String) {
        let (dir, path, home) = test_site();
        (dir, Cms::new(&path), home)
    }

    fn paragraph(text: &str) -> Value {
        json!({ "type": "doc", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": text }] }] })
    }

    #[test]
    fn pages_are_created_listed_changed_and_deleted() {
        let (_dir, cms, home) = site();
        let about = cms
            .create(&json!({ "inside": home, "title": "Chi siamo", "menu": true }))
            .unwrap();
        let about_id = about["id"].as_str().unwrap().to_string();
        assert_eq!(about["slug"], "chi-siamo");
        assert_eq!(about["lang"], "it");
        assert_eq!(about["order"], 0);
        let team = cms
            .create(&json!({ "inside": about_id, "title": "Il gruppo" }))
            .unwrap();
        let news = cms
            .create(&json!({ "inside": home, "title": "Novità" }))
            .unwrap();
        assert_eq!(news["slug"], "novita");
        assert_eq!(news["order"], 1);

        let list = cms.list().unwrap();
        let addresses: Vec<&str> = list.iter().map(|p| p.address.as_str()).collect();
        assert_eq!(
            addresses,
            ["/", "/chi-siamo", "/chi-siamo/il-gruppo", "/novita"]
        );

        // A change keeps what it does not name and writes the canonical form.
        let changed = cms
            .update(
                &home,
                &json!({ "title": "Studio Mancini", "blocks": [{ "id": "a1", "type": "text", "body": paragraph("Benvenuti") }], "seo": { "description": "Architettura a Torino" } }),
            )
            .unwrap();
        assert_eq!(changed["title"], "Studio Mancini");
        assert_eq!(changed["seo"]["hidden"], false);
        assert_eq!(changed["seo"]["description"], "Architettura a Torino");
        let file = std::fs::read_to_string(cms.pages_dir().join(format!("{home}.json"))).unwrap();
        assert!(file.starts_with("{\n  \"id\""));
        assert!(file.ends_with("}\n"));

        // Invalid changes are refused with the reasons, and nothing changes.
        let refused = cms.update(&home, &json!({ "title": "", "slug": "Bad Slug" }));
        let Err(CmsError::Invalid(problems)) = refused else {
            panic!("expected problems")
        };
        assert_eq!(problems.len(), 2);
        assert_eq!(cms.read(&home).unwrap()["title"], "Studio Mancini");
        assert!(matches!(
            cms.update(&home, &json!({ "id": "x" })),
            Err(CmsError::Refused(_))
        ));
        let same_address = cms.update(
            news["id"].as_str().unwrap(),
            &json!({ "slug": "chi-siamo" }),
        );
        assert!(matches!(same_address, Err(CmsError::Invalid(_))));
        let into_itself = cms.update(&about_id, &json!({ "inside": team["id"] }));
        assert!(matches!(into_itself, Err(CmsError::Invalid(_))));

        // Deleting: not the home, not a page with pages inside, not a page
        // others link to.
        assert!(matches!(cms.delete(&home), Err(CmsError::Refused(_))));
        assert!(matches!(cms.delete(&about_id), Err(CmsError::Refused(_))));
        let link = json!({ "type": "doc", "content": [{ "type": "paragraph", "content": [
            { "type": "text", "text": "news", "marks": [{ "type": "link", "attrs": { "page": news["id"] } }] }
        ] }] });
        cms.update(
            &home,
            &json!({ "blocks": [{ "id": "a1", "type": "text", "body": link }] }),
        )
        .unwrap();
        let news_id = news["id"].as_str().unwrap();
        assert!(matches!(cms.delete(news_id), Err(CmsError::Refused(_))));
        cms.update(&home, &json!({ "blocks": [] })).unwrap();
        cms.delete(news_id).unwrap();
        assert_eq!(cms.read(news_id), Err(CmsError::NotFound));
        assert_eq!(cms.list().unwrap().len(), 3);
    }

    #[test]
    fn slugs_come_from_titles() {
        assert_eq!(slug_of("Chi siamo?"), "chi-siamo");
        assert_eq!(slug_of("  Più   Città  "), "piu-citta");
        assert_eq!(slug_of("!!!"), "page");
    }
}
