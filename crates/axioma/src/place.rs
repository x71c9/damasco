//! Places in the panel the assistant can point to ("go to the field"): a
//! section, a page and one of its fields, or a picture. Each is checked
//! against the site and turned into an address of the panel and words that
//! say where it leads.

use serde_json::{Value, json};

use crate::cms::Cms;

/// Where a link of the chat leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The panel's address, such as `/pages/<id>?field=seo.description`.
    pub href: String,
    /// Where that is, in the panel's words: "Pages › Home › Title".
    pub path: String,
}

impl Place {
    pub fn to_json(&self) -> Value {
        json!({ "href": self.href, "place": self.path })
    }
}

/// The fields every page has, with their names in the editor.
const PAGE_FIELDS: [(&str, &str); 8] = [
    ("title", "Title"),
    ("slug", "Address"),
    ("inside", "Parent"),
    ("menu", "Show it in the site menu"),
    ("seo.title", "Title in search results"),
    ("seo.description", "Description in search results"),
    ("seo.image", "Image when shared"),
    ("seo.hidden", "Hide from search engines"),
];

/// Checks the arguments of the tool and finds the place they name.
pub fn find(cms: &Cms, arguments: &Value) -> Result<Place, String> {
    let text = |key: &str| {
        arguments[key]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    match text("section") {
        Some("pages") => page_place(cms, text("page"), text("field")),
        Some("media") => {
            let Some(id) = text("picture") else {
                return Ok(Place {
                    href: "/media".into(),
                    path: "Media".into(),
                });
            };
            let picture = crate::media::read(cms.site(), id)
                .map_err(|_| format!("there is no picture with the id {id:?}: see list_media"))?;
            let name = picture["name"].as_str().unwrap_or("picture");
            Ok(Place {
                href: format!("/media?picture={id}"),
                path: format!("Media \u{203a} {name}"),
            })
        }
        Some("people") => Ok(Place {
            href: "/people".into(),
            path: "People".into(),
        }),
        Some("activity") => Ok(Place {
            href: "/activity".into(),
            path: "Activity".into(),
        }),
        Some(other) => Err(format!(
            "there is no section {other:?}; the sections are pages, media, people and activity"
        )),
        None => Err("say which section: pages, media, people or activity".into()),
    }
}

fn page_place(cms: &Cms, page: Option<&str>, field: Option<&str>) -> Result<Place, String> {
    let Some(id) = page else {
        if field.is_some() {
            return Err("a field needs the page it belongs to".into());
        }
        return Ok(Place {
            href: "/pages".into(),
            path: "Pages".into(),
        });
    };
    let page = cms
        .read(id)
        .map_err(|_| format!("there is no page with the id {id:?}: see list_pages"))?;
    let title = page["title"].as_str().unwrap_or("Untitled page");
    let mut place = Place {
        href: format!("/pages/{id}"),
        path: format!("Pages \u{203a} {title}"),
    };
    let Some(field) = field else {
        return Ok(place);
    };
    let label = field_label(cms, &page, field)?;
    place.href.push_str(&format!("?field={field}"));
    place.path.push_str(&format!(" \u{203a} {label}"));
    Ok(place)
}

/// The name of a field of the page in the editor, if the page has it.
fn field_label(cms: &Cms, page: &Value, field: &str) -> Result<String, String> {
    if let Some((_, label)) = PAGE_FIELDS.iter().find(|(name, _)| *name == field) {
        return Ok((*label).to_string());
    }
    if let Some(name) = field.strip_prefix("fields.") {
        let types = cms.types().map_err(|err| format!("{err:?}"))?;
        let page_type = page["type"].as_str().unwrap_or_default();
        let fields = types
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["name"] == page_type)
            .map(|t| t["fields"].clone())
            .unwrap_or(Value::Null);
        return fields
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["name"] == name)
            .and_then(|f| f["label"].as_str().map(String::from))
            .ok_or_else(|| {
                format!("the page type {page_type:?} has no field {name:?}: see page_types")
            });
    }
    if let Some(block) = field.strip_prefix("blocks.") {
        let blocks = page["blocks"].as_array().cloned().unwrap_or_default();
        let (index, found) = blocks
            .iter()
            .enumerate()
            .find(|(_, b)| b["id"] == block)
            .ok_or_else(|| format!("the page has no block with the id {block:?}"))?;
        let kind = if found["type"] == "media" { "Media" } else { "Text" };
        return Ok(format!("{kind} block {}", index + 1));
    }
    Err(format!(
        "there is no field {field:?}; a field is one of {}, fields.<name of a field of the page type>, or blocks.<id of a block>",
        PAGE_FIELDS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_are_checked_and_named() {
        let (_dir, site, home) = crate::cms::test_site();
        let cms = Cms::new(&site);
        let find = |arguments: Value| find(&cms, &arguments);

        let title = find(json!({ "section": "pages", "page": home, "field": "seo.description" }))
            .unwrap();
        assert_eq!(title.href, format!("/pages/{home}?field=seo.description"));
        assert_eq!(
            title.path,
            "Pages \u{203a} Studio \u{203a} Description in search results"
        );
        assert_eq!(find(json!({ "section": "pages" })).unwrap().href, "/pages");
        assert_eq!(find(json!({ "section": "people" })).unwrap().path, "People");

        // Nothing that does not exist.
        assert!(find(json!({ "section": "pages", "page": home, "field": "colour" }))
            .unwrap_err()
            .contains("there is no field"));
        assert!(find(json!({ "section": "pages", "page": home, "field": "fields.year" }))
            .unwrap_err()
            .contains("no field \"year\""));
        assert!(find(json!({ "section": "pages", "page": crate::site::ulid() }))
            .unwrap_err()
            .contains("no page"));
        assert!(find(json!({ "section": "media", "picture": crate::site::ulid() }))
            .unwrap_err()
            .contains("no picture"));
        assert!(find(json!({ "section": "settings" })).is_err());
        assert!(find(json!({ "section": "pages", "field": "title" })).is_err());
    }
}
