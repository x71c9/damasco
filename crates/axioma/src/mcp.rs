//! axioma's MCP server, run by the CLI inside the agent sandbox.
//!
//! It speaks the Model Context Protocol on standard input and output, one
//! JSON-RPC message per line, and offers the tools the product gives the
//! assistant. Each call goes to axioma through the hook socket; the answer
//! comes back as the tool's result. Today the only tool is `ask`: questions
//! with options, answered by a person in the chat.

use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn run(socket: &Path) -> ExitCode {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications have no id and get no answer.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let result = handle(socket, &message).await;
        let reply = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, text)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": text } })
            }
        };
        if stdout
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .is_err()
            || stdout.flush().await.is_err()
        {
            break;
        }
    }
    ExitCode::SUCCESS
}

async fn handle(socket: &Path, message: &Value) -> Result<Value, (i64, String)> {
    match message["method"].as_str() {
        Some("initialize") => Ok(json!({
            "protocolVersion": message["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "axioma", "version": env!("CARGO_PKG_VERSION") },
        })),
        Some("ping") => Ok(json!({})),
        Some("tools/list") => Ok(json!({ "tools": tools() })),
        Some("tools/call") => Ok(call(socket, &message["params"]).await),
        _ => Err((-32601, "method not found".into())),
    }
}

fn tools() -> Value {
    json!([{
        "name": "ask",
        "description": "Ask the person one to four questions with options, shown in the chat as buttons, and wait for the answers. Use it when a choice is the person's to make: wording, placement, which pages, yes or no. Each question has two to six short options; the person can also answer in their own words. The result lists each question with the answer.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "The question, in the person's language, ending with a question mark." },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 6,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "A few words." },
                                        "description": { "type": "string", "description": "Optional, one short line." }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multiSelect": { "type": "boolean", "description": "True when more than one option can be chosen." }
                        },
                        "required": ["question", "options"]
                    }
                }
            },
            "required": ["questions"]
        }
    }, {
        "name": "list_pages",
        "description": "List the pages of the site in tree order, with id, type, title, slug, the page they are inside, order, whether they are in the menu, and their public address.",
        "inputSchema": { "type": "object", "properties": {} }
    }, {
        "name": "read_page",
        "description": "Read one page: its fields, blocks and search settings, as stored.",
        "inputSchema": id_schema()
    }, {
        "name": "page_types",
        "description": "The page types of the site and their fields. A page's `fields` object holds the values of its type's fields.",
        "inputSchema": { "type": "object", "properties": {} }
    }, {
        "name": "update_page",
        "description": concat!(
            "Change a page. Each key in `changes` replaces the page's value, except `fields` and `seo`, whose keys are merged. ",
            "Keys: type, inside (id of the parent page), order, slug (lowercase letters, digits, dashes), title, menu (true to show it in the site menu), fields, blocks, seo ({title, description, image, hidden}). ",
            "`blocks` is the whole list of blocks, in order. A text block is {\"id\": \"a short unique id\", \"type\": \"text\", \"body\": <rich text>}; ",
            "a media block is {\"id\", \"type\": \"media\", \"media\": null, \"caption\": null} (pictures come later). ",
            "Rich text is a ProseMirror document: {\"type\": \"doc\", \"content\": [{\"type\": \"paragraph\", \"content\": [{\"type\": \"text\", \"text\": \"Hello \"}, {\"type\": \"text\", \"text\": \"world\", \"marks\": [{\"type\": \"bold\"}]}]}]}. ",
            "Nodes: paragraph, heading (attrs.level 2 to 4), bulletList and orderedList of listItem (each holding paragraphs), blockquote, hardBreak, text. ",
            "Marks: bold, italic, code, link (attrs.page with the id of a page, or attrs.href with an https or mailto address). ",
            "The page is checked before it is saved; when something is wrong nothing is saved and the result says what to fix."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The id of the page." },
                "changes": { "type": "object", "description": "The keys to change." }
            },
            "required": ["id", "changes"]
        }
    }, {
        "name": "create_page",
        "description": "Create a page inside another page. `inside` and `title` are needed; the slug comes from the title when missing, and the page goes after the other pages in the same place. Other keys as in update_page. The result is the new page, with its id.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "page": { "type": "object", "description": "The new page: inside, title, and optionally type, slug, order, menu, fields, blocks, seo." }
            },
            "required": ["page"]
        }
    }, {
        "name": "delete_page",
        "description": "Delete a page. The person confirms it in the chat first. The home page, pages with other pages inside and pages other pages link to cannot be deleted.",
        "inputSchema": id_schema()
    }])
}

fn id_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "id": { "type": "string", "description": "The id of the page." } },
        "required": ["id"]
    })
}

/// Passes a tool call to axioma and turns its answer into a tool result.
async fn call(socket: &Path, params: &Value) -> Value {
    let request = json!({ "axioma_tool": params["name"], "arguments": params["arguments"] });
    let answer = crate::hook::exchange(socket, request.to_string().as_bytes())
        .await
        .map_err(|err| format!("axioma did not answer: {err}"))
        .and_then(|bytes| {
            serde_json::from_slice::<Value>(&bytes).map_err(|_| "axioma gave no answer".to_string())
        });
    let (text, error) = match answer {
        Ok(answer) => match answer["error"].as_str() {
            Some(error) => (error.to_string(), true),
            None => (answer["text"].as_str().unwrap_or("").to_string(), false),
        },
        Err(problem) => (problem, true),
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": error })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_the_handshake_and_lists_the_tool() {
        let socket = Path::new("/nonexistent/hooks.sock");
        let init = handle(
            socket,
            &json!({ "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }),
        )
        .await
        .unwrap();
        assert_eq!(init["protocolVersion"], "2025-03-26");
        let list = handle(socket, &json!({ "method": "tools/list" }))
            .await
            .unwrap();
        assert_eq!(list["tools"][0]["name"], "ask");
        assert!(
            handle(socket, &json!({ "method": "resources/list" }))
                .await
                .is_err()
        );
        // Without axioma the call fails as a tool error, not a protocol one.
        let result = handle(
            socket,
            &json!({ "method": "tools/call", "params": { "name": "ask" } }),
        )
        .await
        .unwrap();
        assert_eq!(result["isError"], true);
    }
}
