//! Components: the parts several pages share, such as a header or a footer.
//! Their content lives in `content/components/<id>.json` and is changed
//! only through here, checked and written like the pages; the site's code
//! shows each one where it appears, finding it by its `key`.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::cms::{Cms, CmsError, slug_of};
use crate::schema;

/// A component as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub key: String,
    pub everywhere: bool,
    /// The pages it appears on, chosen one by one.
    pub pages: Vec<String>,
    /// The pages whose pages inside show it.
    pub inside: Vec<String>,
}

/// Keys a change may set. `id` and `key` never change: the site's code
/// finds the component by its key.
const CHANGEABLE: [&str; 5] = ["name", "blocks", "everywhere", "pages", "inside"];

fn missing() -> CmsError {
    CmsError::Refused("no component has this id".into())
}

/// The ids in a list of `{ "page": id }`.
fn page_ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["page"].as_str().map(String::from))
        .collect()
}

impl Cms {
    fn components_dir(&self) -> PathBuf {
        self.site().join("content/components")
    }

    /// Every component, as stored.
    pub fn all_components(&self) -> Result<Vec<Value>, CmsError> {
        let mut components = Vec::new();
        let entries = match std::fs::read_dir(self.components_dir()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(components),
            Err(err) => return Err(CmsError::Io(err.to_string())),
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let text = std::fs::read_to_string(&path).map_err(|e| CmsError::Io(e.to_string()))?;
            let component = serde_json::from_str(&text)
                .map_err(|e| CmsError::Io(format!("{}: {e}", path.display())))?;
            components.push(component);
        }
        Ok(components)
    }

    /// The components, by name.
    pub fn list_components(&self) -> Result<Vec<Summary>, CmsError> {
        let mut list: Vec<Summary> = self
            .all_components()?
            .iter()
            .map(|component| Summary {
                id: component["id"].as_str().unwrap_or_default().to_string(),
                name: component["name"].as_str().unwrap_or_default().to_string(),
                key: component["key"].as_str().unwrap_or_default().to_string(),
                everywhere: component["everywhere"].as_bool().unwrap_or_default(),
                pages: page_ids(&component["pages"]),
                inside: page_ids(&component["inside"]),
            })
            .collect();
        list.sort_by_key(|summary| summary.name.to_lowercase());
        Ok(list)
    }

    pub fn read_component(&self, id: &str) -> Result<Value, CmsError> {
        if !schema::is_ulid(id) {
            return Err(missing());
        }
        let text = std::fs::read_to_string(self.components_dir().join(format!("{id}.json")))
            .map_err(|_| missing())?;
        serde_json::from_str(&text).map_err(|e| CmsError::Io(e.to_string()))
    }

    /// Creates a component. `name` is needed; the key comes from the name
    /// when missing, and a new component appears on every page unless told
    /// otherwise.
    pub fn create_component(&self, given: &Value) -> Result<Value, CmsError> {
        let given = given
            .as_object()
            .ok_or_else(|| CmsError::Refused("the component must be an object".into()))?;
        for key in given.keys() {
            if key != "key" && !CHANGEABLE.contains(&key.as_str()) {
                return Err(CmsError::Refused(format!(
                    "{key:?} cannot be set; the keys are key, {}",
                    CHANGEABLE.join(", ")
                )));
            }
        }
        let name = given
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut component = Map::new();
        component.insert("id".into(), json!(crate::site::ulid()));
        component.insert("name".into(), json!(name));
        component.insert("key".into(), json!(slug_of(name)));
        component.insert("blocks".into(), json!([]));
        component.insert("everywhere".into(), json!(true));
        component.insert("pages".into(), json!([]));
        component.insert("inside".into(), json!([]));
        for (key, value) in given {
            component.insert(key.clone(), value.clone());
        }
        self.save_component(Value::Object(component))
    }

    /// Applies `changes`: each key given replaces the component's value.
    pub fn update_component(&self, id: &str, changes: &Value) -> Result<Value, CmsError> {
        let mut component = self.read_component(id)?;
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
            component[key] = value.clone();
        }
        self.save_component(component)
    }

    /// Removes a component. The site's code that shows it then finds
    /// nothing and shows nothing.
    pub fn delete_component(&self, id: &str) -> Result<Value, CmsError> {
        let component = self.read_component(id)?;
        std::fs::remove_file(self.components_dir().join(format!("{id}.json")))
            .map_err(|e| CmsError::Io(e.to_string()))?;
        Ok(component)
    }

    /// The components that name the page `id` among the pages they appear on.
    pub fn components_on(&self, id: &str) -> Result<Vec<String>, CmsError> {
        Ok(self
            .list_components()?
            .into_iter()
            .filter(|c| c.pages.iter().chain(&c.inside).any(|page| page == id))
            .map(|c| c.name)
            .collect())
    }

    /// Checks a component with the pages and the other components, and
    /// writes it.
    fn save_component(&self, component: Value) -> Result<Value, CmsError> {
        let id = component["id"].as_str().unwrap_or_default().to_string();
        let at = format!("content/components/{id}.json");
        let pages: HashSet<String> = self.list()?.into_iter().map(|page| page.id).collect();
        let mut problems = Vec::new();
        schema::check_component(&at, &component, &pages, &mut problems);
        schema::check_media(
            &at,
            &component,
            &schema::media_ids(self.site()),
            &mut problems,
        );
        let mut all: Vec<(String, Value)> = self
            .all_components()?
            .into_iter()
            .filter(|c| c["id"].as_str() != Some(&id))
            .map(|c| {
                let at = format!(
                    "content/components/{}.json",
                    c["id"].as_str().unwrap_or_default()
                );
                (at, c)
            })
            .collect();
        all.push((at, component.clone()));
        schema::check_component_keys(&all, &mut problems);
        if !problems.is_empty() {
            return Err(CmsError::Invalid(problems));
        }
        let text = schema::canonical_component(&component);
        let dir = self.components_dir();
        std::fs::create_dir_all(&dir).map_err(|e| CmsError::Io(e.to_string()))?;
        let temporary = dir.join(format!(".{id}.json.new"));
        std::fs::write(&temporary, &text).map_err(|e| CmsError::Io(e.to_string()))?;
        std::fs::rename(&temporary, dir.join(format!("{id}.json")))
            .map_err(|e| CmsError::Io(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| CmsError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(words: &str) -> Value {
        json!({ "type": "doc", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": words }] }] })
    }

    #[test]
    fn components_are_created_changed_and_removed() {
        let (_dir, site, home) = crate::cms::test_site();
        let cms = Cms::new(&site);
        assert!(cms.list_components().unwrap().is_empty());

        let footer = cms
            .create_component(&json!({ "name": "Footer", "blocks": [{ "id": "b1", "type": "text", "body": text("Via Volta 12") }] }))
            .unwrap();
        let id = footer["id"].as_str().unwrap().to_string();
        assert_eq!(footer["key"], "footer");
        assert_eq!(footer["everywhere"], true);
        let file =
            std::fs::read_to_string(site.join(format!("content/components/{id}.json"))).unwrap();
        assert!(file.starts_with("{\n  \"id\""));

        // Shown only inside the home: the page is named, so it stays.
        let changed = cms
            .update_component(
                &id,
                &json!({ "everywhere": false, "inside": [{ "page": home }] }),
            )
            .unwrap();
        assert_eq!(changed["inside"][0]["page"], home.as_str());
        assert_eq!(cms.components_on(&home).unwrap(), ["Footer"]);
        let listed = cms.list_components().unwrap();
        assert_eq!(
            (listed[0].everywhere, listed[0].inside.clone()),
            (false, vec![home.clone()])
        );

        // Nothing that does not hold.
        assert!(matches!(
            cms.update_component(&id, &json!({ "pages": [{ "page": crate::site::ulid() }] })),
            Err(CmsError::Invalid(_))
        ));
        assert!(matches!(
            cms.update_component(&id, &json!({ "key": "other" })),
            Err(CmsError::Refused(_))
        ));
        assert!(matches!(
            cms.create_component(&json!({ "name": "Another footer", "key": "footer" })),
            Err(CmsError::Invalid(_))
        ));
        assert!(matches!(
            cms.create_component(&json!({ "name": "" })),
            Err(CmsError::Invalid(_))
        ));
        assert!(matches!(
            cms.update_component(&id, &json!({ "blocks": [{ "id": "b1", "type": "media", "media": { "media": crate::site::ulid() }, "caption": null }] })),
            Err(CmsError::Invalid(_))
        ));

        // The whole site is checked with its components.
        let schema = schema::Schema::load(&site).unwrap();
        assert!(schema.check_site(&site).is_empty());

        cms.delete_component(&id).unwrap();
        assert!(cms.read_component(&id).is_err());
        assert!(cms.list_components().unwrap().is_empty());
    }
}
