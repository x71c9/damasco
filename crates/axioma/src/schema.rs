//! Page types and the page format.
//!
//! Everything on a site is a page. A page has a fixed structure (title,
//! address, place in the tree, blocks, search settings) and the fields of its
//! type. Types live in `schema/types/<name>.json` of the site repository:
//!
//! ```json
//! { "name": "project", "label": "Project",
//!   "fields": [ { "name": "year", "label": "Year", "type": "number" },
//!               { "name": "gallery", "label": "Gallery", "type": "list", "of": { "type": "media" } } ] }
//! ```
//!
//! From the types axioma validates every page, writes pages in one canonical
//! form, and generates the Zod schema Astro reads them with.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A field of a page type. Unknown keys are refused by `Kind`, which
/// receives every key `Field` does not use (serde cannot refuse them here,
/// next to a flattened field).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Field {
    pub name: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: Kind,
    #[serde(default)]
    pub required: bool,
    /// One line shown under the field in the panel.
    #[serde(default)]
    pub help: Option<String>,
}

/// The value a field holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Kind {
    Text {
        #[serde(default)]
        max: Option<usize>,
    },
    Richtext,
    Number,
    Boolean,
    /// `YYYY-MM-DD`.
    Date,
    /// `{ "media": "<id>" }`.
    Media,
    /// `{ "page": "<id>" }`.
    Page,
    List {
        of: Box<Item>,
    },
    Group {
        fields: Vec<Field>,
    },
}

/// The element type of a list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Item {
    #[serde(flatten)]
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageType {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// One problem, with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The file or page, then the path inside it: `title`, `fields.year`.
    pub at: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

/// The page types of a site.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub types: BTreeMap<String, PageType>,
}

const BASE_KEYS: [&str; 11] = [
    "id", "type", "lang", "inside", "order", "slug", "title", "menu", "fields", "blocks", "seo",
];
const SEO_KEYS: [&str; 4] = ["title", "description", "image", "hidden"];

impl Schema {
    /// Reads `schema/types/*.json`. The type `page` always exists.
    pub fn load(site: &Path) -> Result<Self, Vec<Problem>> {
        let mut schema = Schema::default();
        let mut problems = Vec::new();
        let dir = site.join("schema/types");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for path in files
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let at = format!(
                "schema/types/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()))
                .and_then(|raw| {
                    for field in raw
                        .get("fields")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        definition_keys(field, true)?;
                    }
                    serde_json::from_value::<PageType>(raw).map_err(|e| e.to_string())
                });
            match parsed {
                Ok(page_type) => {
                    let expected = path.file_stem().unwrap_or_default().to_string_lossy();
                    if page_type.name != expected {
                        problems.push(problem(
                            &at,
                            format!("the name must be {expected:?}, like the file"),
                        ));
                    }
                    check_fields(&at, &page_type.fields, &mut problems);
                    schema.types.insert(page_type.name.clone(), page_type);
                }
                Err(err) => problems.push(problem(&at, err)),
            }
        }
        schema
            .types
            .entry("page".into())
            .or_insert_with(|| PageType {
                name: "page".into(),
                label: "Page".into(),
                fields: Vec::new(),
            });
        if problems.is_empty() {
            Ok(schema)
        } else {
            Err(problems)
        }
    }

    /// Checks one page on its own: structure, fields of its type, blocks.
    pub fn check_page(&self, at: &str, page: &Value, problems: &mut Vec<Problem>) {
        let Some(object) = page.as_object() else {
            problems.push(problem(at, "a page is a JSON object"));
            return;
        };
        for key in object.keys() {
            if !BASE_KEYS.contains(&key.as_str()) {
                problems.push(problem(at, format!("unknown key {key:?}")));
            }
        }
        let field = |name: &str| object.get(name).unwrap_or(&Value::Null);
        let path = |name: &str| format!("{at} {name}");
        if !field("id").as_str().is_some_and(is_ulid) {
            problems.push(problem(&path("id"), "expected a ULID of 26 characters"));
        }
        if field("lang").as_str().is_none_or(str::is_empty) {
            problems.push(problem(&path("lang"), "expected the language code"));
        }
        let inside = field("inside");
        if !(inside.is_null() || inside.as_str().is_some_and(is_ulid)) {
            problems.push(problem(
                &path("inside"),
                "expected null or the id of a page",
            ));
        }
        if !field("order").is_i64() {
            problems.push(problem(&path("order"), "expected a whole number"));
        }
        match field("slug").as_str() {
            Some("") if inside.is_null() => {}
            Some("") => problems.push(problem(
                &path("slug"),
                "only the home page has an empty address",
            )),
            Some(slug) if is_slug(slug) => {}
            _ => problems.push(problem(
                &path("slug"),
                "expected lowercase letters, digits and single dashes, such as about-us",
            )),
        }
        if field("title").as_str().is_none_or(|s| s.trim().is_empty()) {
            problems.push(problem(&path("title"), "expected a title"));
        }
        if !field("menu").is_boolean() {
            problems.push(problem(&path("menu"), "expected true or false"));
        }
        match field("type").as_str().and_then(|t| self.types.get(t)) {
            Some(page_type) => check_values(
                &path("fields"),
                &page_type.fields,
                field("fields"),
                problems,
            ),
            None => problems.push(problem(&path("type"), "unknown page type")),
        }
        check_blocks(&path("blocks"), field("blocks"), problems);
        check_seo(&path("seo"), field("seo"), problems);
    }

    /// Checks every page of the site, and the tree they form.
    pub fn check_site(&self, site: &Path) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut pages: Vec<(String, Value)> = Vec::new();
        let dir = site.join("content/pages");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for path in files
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let at = format!(
                "content/pages/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()))
            {
                Ok(page) => {
                    let stem = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    if page.get("id").and_then(Value::as_str) != Some(&stem) {
                        problems.push(problem(&at, "the id must match the file name"));
                    }
                    self.check_page(&at, &page, &mut problems);
                    pages.push((at, page));
                }
                Err(err) => problems.push(problem(&at, err)),
            }
        }
        check_tree(&pages, &mut problems);
        problems
    }
}

fn problem(at: &str, message: impl Into<String>) -> Problem {
    Problem {
        at: at.to_string(),
        message: message.into(),
    }
}

pub fn is_ulid(text: &str) -> bool {
    text.len() == 26
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c)))
}

fn is_slug(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 100
        && text.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && text.len() <= 40
}

/// Refuses keys a field definition does not use, at every depth. serde
/// cannot do it for these flattened, tagged definitions.
fn definition_keys(value: &Value, named: bool) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err("a field definition is an object".into());
    };
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut allowed: Vec<&str> = vec!["type"];
    if named {
        allowed.extend(["name", "label", "required", "help"]);
    }
    match kind {
        "text" => allowed.push("max"),
        "list" => allowed.push("of"),
        "group" => allowed.push("fields"),
        _ => {}
    }
    if let Some(key) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        let name = object.get("name").and_then(Value::as_str).unwrap_or(kind);
        return Err(format!("{name}: unknown key {key:?}"));
    }
    if let Some(of) = object.get("of") {
        definition_keys(of, false)?;
    }
    for field in object
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        definition_keys(field, true)?;
    }
    Ok(())
}

/// Checks the definition of fields: names, uniqueness, nesting.
fn check_fields(at: &str, fields: &[Field], problems: &mut Vec<Problem>) {
    let mut seen = HashSet::new();
    for field in fields {
        let here = format!("{at} {}", field.name);
        if !is_name(&field.name) {
            problems.push(problem(
                &here,
                "field names use lowercase letters, digits and underscores",
            ));
        }
        if !seen.insert(field.name.as_str()) {
            problems.push(problem(&here, "the field is defined twice"));
        }
        check_kind(&here, &field.kind, problems);
    }
}

fn check_kind(at: &str, kind: &Kind, problems: &mut Vec<Problem>) {
    match kind {
        Kind::Group { fields } => check_fields(at, fields, problems),
        Kind::List { of } => {
            if matches!(of.kind, Kind::List { .. }) {
                problems.push(problem(at, "a list cannot contain lists"));
            }
            check_kind(at, &of.kind, problems);
        }
        _ => {}
    }
}

/// Checks the values of `fields` against their definitions.
fn check_values(at: &str, fields: &[Field], value: &Value, problems: &mut Vec<Problem>) {
    let Some(object) = value.as_object() else {
        problems.push(problem(at, "expected an object"));
        return;
    };
    for key in object.keys() {
        if !fields.iter().any(|f| &f.name == key) {
            problems.push(problem(
                &format!("{at}.{key}"),
                "not a field of this page type",
            ));
        }
    }
    for field in fields {
        let here = format!("{at}.{}", field.name);
        match object.get(&field.name) {
            None | Some(Value::Null) if field.required => problems.push(problem(&here, "required")),
            None | Some(Value::Null) => {}
            Some(value) => check_value(&here, &field.kind, value, problems),
        }
    }
}

fn check_value(at: &str, kind: &Kind, value: &Value, problems: &mut Vec<Problem>) {
    let ok = match kind {
        Kind::Text { max } => match value.as_str() {
            Some(text) => max.is_none_or(|max| text.chars().count() <= max),
            None => false,
        },
        Kind::Richtext => {
            check_richtext(at, value, problems);
            true
        }
        Kind::Number => value.is_number(),
        Kind::Boolean => value.is_boolean(),
        Kind::Date => value.as_str().is_some_and(is_date),
        Kind::Media => reference(value, "media"),
        Kind::Page => reference(value, "page"),
        Kind::List { of } => match value.as_array() {
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    check_value(&format!("{at}[{index}]"), &of.kind, item, problems);
                }
                true
            }
            None => false,
        },
        Kind::Group { fields } => {
            check_values(at, fields, value, problems);
            true
        }
    };
    if !ok {
        problems.push(problem(at, format!("expected {}", describe(kind))));
    }
}

fn describe(kind: &Kind) -> &'static str {
    match kind {
        Kind::Text { .. } => "text, within the length allowed",
        Kind::Richtext => "rich text",
        Kind::Number => "a number",
        Kind::Boolean => "true or false",
        Kind::Date => "a date such as 2026-11-16",
        Kind::Media => "{ \"media\": \"<id>\" }",
        Kind::Page => "{ \"page\": \"<id>\" }",
        Kind::List { .. } => "a list",
        Kind::Group { .. } => "an object",
    }
}

fn reference(value: &Value, key: &str) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == 1 && object.get(key).and_then(Value::as_str).is_some_and(is_ulid)
    })
}

fn is_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
        && matches!(parts[1].parse::<u8>(), Ok(1..=12))
        && matches!(parts[2].parse::<u8>(), Ok(1..=31))
}

fn check_blocks(at: &str, value: &Value, problems: &mut Vec<Problem>) {
    let Some(blocks) = value.as_array() else {
        problems.push(problem(at, "expected a list of blocks"));
        return;
    };
    let mut ids = HashSet::new();
    for (index, block) in blocks.iter().enumerate() {
        let here = format!("{at}[{index}]");
        let Some(object) = block.as_object() else {
            problems.push(problem(&here, "a block is an object"));
            continue;
        };
        match object.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() && id.len() <= 32 => {
                if !ids.insert(id) {
                    problems.push(problem(&here, "two blocks have the same id"));
                }
            }
            _ => problems.push(problem(&here, "expected an id")),
        }
        let allowed: &[&str] = match object.get("type").and_then(Value::as_str) {
            Some("text") => {
                check_richtext(
                    &format!("{here}.body"),
                    object.get("body").unwrap_or(&Value::Null),
                    problems,
                );
                &["id", "type", "body"]
            }
            Some("media") => {
                let media = object.get("media").unwrap_or(&Value::Null);
                if !(media.is_null() || reference(media, "media")) {
                    problems.push(problem(
                        &format!("{here}.media"),
                        "expected null or { \"media\": \"<id>\" }",
                    ));
                }
                let caption = object.get("caption").unwrap_or(&Value::Null);
                if !(caption.is_null() || caption.is_string()) {
                    problems.push(problem(&format!("{here}.caption"), "expected null or text"));
                }
                &["id", "type", "media", "caption"]
            }
            _ => {
                problems.push(problem(&here, "blocks are of type text or media"));
                continue;
            }
        };
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                problems.push(problem(&here, format!("unknown key {key:?}")));
            }
        }
    }
}

fn check_seo(at: &str, value: &Value, problems: &mut Vec<Problem>) {
    let Some(object) = value.as_object() else {
        problems.push(problem(at, "expected an object"));
        return;
    };
    for key in object.keys() {
        if !SEO_KEYS.contains(&key.as_str()) {
            problems.push(problem(at, format!("unknown key {key:?}")));
        }
    }
    for key in ["title", "description"] {
        let value = object.get(key).unwrap_or(&Value::Null);
        if !(value.is_null() || value.is_string()) {
            problems.push(problem(&format!("{at}.{key}"), "expected null or text"));
        }
    }
    let image = object.get("image").unwrap_or(&Value::Null);
    if !(image.is_null() || reference(image, "media")) {
        problems.push(problem(
            &format!("{at}.image"),
            "expected null or { \"media\": \"<id>\" }",
        ));
    }
    if !object.get("hidden").is_some_and(Value::is_boolean) {
        problems.push(problem(&format!("{at}.hidden"), "expected true or false"));
    }
}

/// Rich text: a ProseMirror document limited to the nodes and marks axioma
/// renders. Internal links carry `page`, external links carry `href`.
fn check_richtext(at: &str, value: &Value, problems: &mut Vec<Problem>) {
    if value.get("type").and_then(Value::as_str) != Some("doc") {
        problems.push(problem(at, "expected a rich text document"));
        return;
    }
    check_node(at, value, problems);
}

fn check_node(at: &str, node: &Value, problems: &mut Vec<Problem>) {
    const NODES: [&str; 9] = [
        "doc",
        "paragraph",
        "heading",
        "bulletList",
        "orderedList",
        "listItem",
        "blockquote",
        "hardBreak",
        "text",
    ];
    let kind = node.get("type").and_then(Value::as_str).unwrap_or_default();
    if !NODES.contains(&kind) {
        problems.push(problem(at, format!("rich text does not allow {kind:?}")));
        return;
    }
    if kind == "text" {
        if !node.get("text").is_some_and(Value::is_string) {
            problems.push(problem(at, "a text node needs its text"));
        }
        for mark in node
            .get("marks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match mark.get("type").and_then(Value::as_str) {
                Some("bold" | "italic" | "code") => {}
                Some("link") => {
                    let attrs = mark.get("attrs");
                    let page = attrs.and_then(|a| a.get("page")).and_then(Value::as_str);
                    let href = attrs.and_then(|a| a.get("href")).and_then(Value::as_str);
                    let ok = page.is_some_and(is_ulid)
                        || href.is_some_and(|h| {
                            h.starts_with("https://")
                                || h.starts_with("http://")
                                || h.starts_with("mailto:")
                        });
                    if !ok {
                        problems.push(problem(
                            at,
                            "a link needs the id of a page or an http(s) or mailto address",
                        ));
                    }
                }
                other => problems.push(problem(
                    at,
                    format!("rich text does not allow the mark {other:?}"),
                )),
            }
        }
    }
    for child in node
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        check_node(at, child, problems);
    }
}

/// One home; every other page inside an existing page; no loops; distinct
/// addresses among siblings.
pub fn check_tree(pages: &[(String, Value)], problems: &mut Vec<Problem>) {
    let inside_of: HashMap<&str, Option<&str>> = pages
        .iter()
        .filter_map(|(_, p)| {
            Some((
                p.get("id")?.as_str()?,
                p.get("inside").and_then(Value::as_str),
            ))
        })
        .collect();
    let homes: Vec<&str> = pages
        .iter()
        .filter(|(_, p)| p.get("inside").is_some_and(Value::is_null))
        .map(|(at, _)| at.as_str())
        .collect();
    if homes.len() != 1 {
        problems.push(problem(
            "content/pages",
            format!("expected one home page, found {}", homes.len()),
        ));
    }
    let mut addresses: HashSet<(Option<&str>, &str)> = HashSet::new();
    for (at, page) in pages {
        let id = page.get("id").and_then(Value::as_str).unwrap_or_default();
        let inside = page.get("inside").and_then(Value::as_str);
        if let Some(parent) = inside
            && !inside_of.contains_key(parent)
        {
            problems.push(problem(&format!("{at} inside"), "no page has this id"));
        }
        // Walk up the tree; more steps than pages means a loop.
        let mut current = inside;
        for _ in 0..=pages.len() {
            match current {
                Some(parent) if parent == id => {
                    problems.push(problem(
                        &format!("{at} inside"),
                        "the page ends up inside itself",
                    ));
                    break;
                }
                Some(parent) => current = inside_of.get(parent).copied().flatten(),
                None => break,
            }
        }
        let slug = page.get("slug").and_then(Value::as_str).unwrap_or_default();
        if !addresses.insert((inside, slug)) {
            problems.push(problem(
                &format!("{at} slug"),
                "another page in the same place has this address",
            ));
        }
    }
}

/// The page in canonical form: keys in a fixed order, fields in the order of
/// their type, two-space indentation, a newline at the end. Diffs then show
/// real changes only.
pub fn canonical(schema: &Schema, page: &Value) -> String {
    let empty = Map::new();
    let object = page.as_object().unwrap_or(&empty);
    let mut out = Map::new();
    for key in BASE_KEYS {
        let value = object.get(key).cloned().unwrap_or(Value::Null);
        let value = match key {
            "fields" => {
                let fields = object
                    .get("type")
                    .and_then(Value::as_str)
                    .and_then(|t| schema.types.get(t))
                    .map(|t| t.fields.as_slice())
                    .unwrap_or_default();
                order_fields(fields, &value)
            }
            "blocks" => Value::Array(
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|block| order_keys(block, &["id", "type", "body", "media", "caption"]))
                    .collect(),
            ),
            "seo" => order_keys(&value, &SEO_KEYS),
            _ => value,
        };
        out.insert(key.to_string(), value);
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(out)).expect("json");
    text.push('\n');
    text
}

fn order_keys(value: &Value, keys: &[&str]) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let mut out = Map::new();
    for key in keys {
        if let Some(v) = object.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    for (key, v) in object {
        if !out.contains_key(key) {
            out.insert(key.clone(), v.clone());
        }
    }
    Value::Object(out)
}

fn order_fields(fields: &[Field], value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return Value::Object(Map::new());
    };
    let mut out = Map::new();
    for field in fields {
        if let Some(v) = object.get(&field.name) {
            let v = match &field.kind {
                Kind::Group { fields } => order_fields(fields, v),
                Kind::List { of } => match (&of.kind, v.as_array()) {
                    (Kind::Group { fields }, Some(items)) => Value::Array(
                        items
                            .iter()
                            .map(|item| order_fields(fields, item))
                            .collect(),
                    ),
                    _ => v.clone(),
                },
                _ => v.clone(),
            };
            out.insert(field.name.clone(), v);
        }
    }
    Value::Object(out)
}

/// TypeScript module with the Zod schema of the pages, for
/// `src/content.config.ts` of the site.
pub fn zod(schema: &Schema) -> String {
    let mut out = String::from(
        "// Generated by axioma from schema/types: do not edit.\n\
         import { z } from 'astro/zod'\n\n\
         const media = z.object({ media: z.string() })\n\
         const page = z.object({ page: z.string() })\n\n\
         const base = {\n  id: z.string(),\n  lang: z.string(),\n  inside: z.string().nullable(),\n  order: z.number(),\n  \
         slug: z.string(),\n  title: z.string(),\n  menu: z.boolean(),\n  blocks: z.array(\n    z.discriminatedUnion('type', [\n      \
         z.object({ id: z.string(), type: z.literal('text'), body: z.any() }),\n      \
         z.object({ id: z.string(), type: z.literal('media'), media: media.nullable(), caption: z.string().nullable().optional() }),\n    \
         ]),\n  ),\n  seo: z.object({\n    title: z.string().nullable(),\n    description: z.string().nullable(),\n    \
         image: media.nullable(),\n    hidden: z.boolean(),\n  }),\n}\n\n",
    );
    out.push_str("export const pageSchema = z.discriminatedUnion('type', [\n");
    for page_type in schema.types.values() {
        out.push_str(&format!(
            "  z.object({{ ...base, type: z.literal({:?}), fields: {} }}),\n",
            page_type.name,
            zod_object(&page_type.fields)
        ));
    }
    out.push_str("])\n");
    out
}

fn zod_object(fields: &[Field]) -> String {
    let parts: Vec<String> = fields
        .iter()
        .map(|field| {
            let base = zod_kind(&field.kind);
            let value = if field.required {
                base
            } else {
                format!("{base}.nullable().optional()")
            };
            format!("{}: {value}", field.name)
        })
        .collect();
    format!("z.object({{ {} }})", parts.join(", "))
}

fn zod_kind(kind: &Kind) -> String {
    match kind {
        Kind::Text { .. } | Kind::Date => "z.string()".into(),
        Kind::Richtext => "z.any()".into(),
        Kind::Number => "z.number()".into(),
        Kind::Boolean => "z.boolean()".into(),
        Kind::Media => "media".into(),
        Kind::Page => "page".into(),
        Kind::List { of } => format!("z.array({})", zod_kind(&of.kind)),
        Kind::Group { fields } => zod_object(fields),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOME: &str = "01KA0000000000000000000000";
    const PROJECTS: &str = "01KA0000000000000000000001";
    const MEDIA: &str = "01KA00000000000000000000MD";

    fn site() -> (tempfile::TempDir, Schema) {
        let dir = tempfile::tempdir().unwrap();
        let types = dir.path().join("schema/types");
        std::fs::create_dir_all(&types).unwrap();
        std::fs::write(
            types.join("project.json"),
            r#"{ "name": "project", "label": "Project", "fields": [
                { "name": "year", "label": "Year", "type": "number", "required": true },
                { "name": "place", "label": "Place", "type": "text", "max": 40 },
                { "name": "gallery", "label": "Gallery", "type": "list", "of": { "type": "media" } },
                { "name": "credits", "label": "Credits", "type": "group", "fields": [
                    { "name": "architect", "label": "Architect", "type": "text" } ] } ] }"#,
        )
        .unwrap();
        let schema = Schema::load(dir.path()).unwrap();
        (dir, schema)
    }

    fn page(id: &str, inside: Option<&str>, slug: &str) -> Value {
        json!({
            "id": id, "type": "page", "lang": "it", "inside": inside, "order": 0, "slug": slug,
            "title": "Titolo", "menu": true, "fields": {}, "blocks": [],
            "seo": { "title": null, "description": null, "image": null, "hidden": false }
        })
    }

    fn problems(schema: &Schema, value: &Value) -> Vec<String> {
        let mut found = Vec::new();
        schema.check_page("p", value, &mut found);
        found.into_iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn loads_types_and_always_has_page() {
        let (_dir, schema) = site();
        assert_eq!(schema.types.keys().collect::<Vec<_>>(), ["page", "project"]);
    }

    #[test]
    fn a_valid_project_passes() {
        let (_dir, schema) = site();
        let mut value = page(PROJECTS, Some(HOME), "casa-sul-lago");
        value["type"] = json!("project");
        value["fields"] = json!({ "year": 2026, "place": "Como", "gallery": [{ "media": MEDIA }], "credits": { "architect": "Studio" } });
        value["blocks"] = json!([
            { "id": "a1", "type": "text", "body": { "type": "doc", "content": [
                { "type": "paragraph", "content": [ { "type": "text", "text": "x", "marks": [ { "type": "link", "attrs": { "page": HOME } } ] } ] } ] } },
            { "id": "b2", "type": "media", "media": { "media": MEDIA }, "caption": null }
        ]);
        assert_eq!(problems(&schema, &value), Vec::<String>::new());
    }

    #[test]
    fn reports_wrong_fields_and_structure() {
        let (_dir, schema) = site();
        let mut value = page(PROJECTS, Some(HOME), "Casa Sul Lago");
        value["type"] = json!("project");
        value["fields"] =
            json!({ "place": "a very long place name that goes on and on", "colour": "red" });
        value["extra"] = json!(1);
        value["blocks"] = json!([{ "id": "a", "type": "html", "html": "<p>" }]);
        let found = problems(&schema, &value).join("\n");
        for expected in [
            "unknown key \"extra\"",
            "p slug: expected lowercase",
            "p fields.year: required",
            "p fields.place: expected text, within the length allowed",
            "p fields.colour: not a field",
            "blocks are of type text or media",
        ] {
            assert!(
                found.contains(expected),
                "missing {expected:?} in:\n{found}"
            );
        }
    }

    #[test]
    fn rich_text_is_restricted() {
        let (_dir, schema) = site();
        let mut value = page(HOME, None, "");
        value["blocks"] = json!([{ "id": "a", "type": "text", "body": { "type": "doc", "content": [
            { "type": "image", "attrs": { "src": "x" } },
            { "type": "paragraph", "content": [ { "type": "text", "text": "x", "marks": [ { "type": "link", "attrs": { "href": "javascript:alert(1)" } } ] } ] }
        ] } }]);
        let found = problems(&schema, &value).join("\n");
        assert!(found.contains("does not allow \"image\""), "{found}");
        assert!(found.contains("a link needs"), "{found}");
    }

    #[test]
    fn checks_the_tree() {
        let (dir, schema) = site();
        let pages = dir.path().join("content/pages");
        std::fs::create_dir_all(&pages).unwrap();
        let write = |value: &Value| {
            let id = value["id"].as_str().unwrap();
            std::fs::write(pages.join(format!("{id}.json")), value.to_string()).unwrap();
        };
        write(&page(HOME, None, ""));
        write(&page(PROJECTS, Some(HOME), "projects"));
        assert_eq!(schema.check_site(dir.path()), Vec::new());
        write(&page("01KA0000000000000000000002", Some(HOME), "projects"));
        write(&page(
            "01KA0000000000000000000003",
            Some("01KA0000000000000000000009"),
            "orphan",
        ));
        write(&page("01KA0000000000000000000004", None, ""));
        let found: Vec<String> = schema
            .check_site(dir.path())
            .iter()
            .map(ToString::to_string)
            .collect();
        let found = found.join("\n");
        assert!(found.contains("expected one home page, found 2"), "{found}");
        assert!(found.contains("no page has this id"), "{found}");
        assert!(found.contains("same place has this address"), "{found}");
    }

    #[test]
    fn canonical_form_is_stable_and_ordered() {
        let (_dir, schema) = site();
        let mut value = page(PROJECTS, Some(HOME), "casa");
        value["type"] = json!("project");
        value["fields"] = json!({ "place": "Como", "year": 2026 });
        let shuffled: Value = serde_json::from_str(&format!(
            r#"{{ "seo": {seo}, "title": "Titolo", "id": "{PROJECTS}", "fields": {{ "place": "Como", "year": 2026 }},
                 "blocks": [], "menu": true, "slug": "casa", "order": 0, "inside": "{HOME}", "lang": "it", "type": "project" }}"#,
            seo = value["seo"]
        ))
        .unwrap();
        let text = canonical(&schema, &shuffled);
        assert_eq!(text, canonical(&schema, &value));
        assert!(text.ends_with("}\n"));
        let year = text.find("\"year\"").unwrap();
        let place = text.find("\"place\"").unwrap();
        assert!(year < place, "fields follow the order of the type");
        assert!(text.find("\"id\"").unwrap() < text.find("\"type\"").unwrap());
    }

    #[test]
    fn generates_zod_for_every_type() {
        let (_dir, schema) = site();
        let code = zod(&schema);
        assert!(code.contains("type: z.literal(\"project\"), fields: z.object({ year: z.number(), place: z.string().nullable().optional()"));
        assert!(code.contains("gallery: z.array(media).nullable().optional()"));
        assert!(code.contains("type: z.literal(\"page\"), fields: z.object({  })"));
    }

    #[test]
    fn refuses_unknown_keys_in_field_definitions() {
        let dir = tempfile::tempdir().unwrap();
        let types = dir.path().join("schema/types");
        std::fs::create_dir_all(&types).unwrap();
        std::fs::write(
            types.join("event.json"),
            r#"{ "name": "event", "label": "Event", "fields": [ { "name": "day", "label": "Day", "type": "date", "colour": "red" } ] }"#,
        )
        .unwrap();
        let found = Schema::load(dir.path()).unwrap_err();
        assert!(found[0].message.contains("colour"), "{}", found[0]);
    }
}
