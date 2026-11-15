//! Read-only Notion API client. The app never writes to Notion (PRD §3).

use std::time::Duration;

use serde_json::Value;

use crate::Result;

const API: &str = "https://api.notion.com/v1";
const VERSION: &str = "2022-06-28";
const KEYCHAIN_SERVICE: &str = "hd-egm-workflow";
const KEYCHAIN_ACCOUNT: &str = "notion";

/// A block plus its already-fetched children.
#[derive(Debug, Clone)]
pub struct Block {
    pub value: Value,
    pub children: Vec<Block>,
}

impl Block {
    pub fn kind(&self) -> &str {
        self.value["type"].as_str().unwrap_or("")
    }

    /// The type-specific payload, e.g. `value["paragraph"]`.
    pub fn data(&self) -> &Value {
        &self.value[self.kind()]
    }

    pub fn id(&self) -> &str {
        self.value["id"].as_str().unwrap_or("")
    }

    /// Builds a block tree from JSON whose blocks carry an inline `children`
    /// array. Used by tests and fixtures.
    pub fn from_json(mut value: Value) -> Block {
        let children = match value.as_object_mut().and_then(|o| o.remove("children")) {
            Some(Value::Array(items)) => items.into_iter().map(Block::from_json).collect(),
            _ => Vec::new(),
        };
        Block { value, children }
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    pub id: String,
    pub title: String,
    pub last_edited_time: String,
}

pub struct Notion {
    http: reqwest::Client,
    token: String,
}

impl Notion {
    /// Reads the integration token from the macOS keychain via
    /// `/usr/bin/security`. The item is created with that same tool, so the
    /// keychain trusts it and never prompts, even after a rebuild changes
    /// this binary's signature (a crate-based read would prompt every time).
    pub fn from_keychain() -> Result<Self> {
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", KEYCHAIN_ACCOUNT, "-w"])
            .output()?;
        let token = String::from_utf8(out.stdout)?.trim_end().to_string();
        if !out.status.success() || token.is_empty() {
            return Err(format!(
                "no Notion token in keychain; store it with: \
                 security add-generic-password -s {KEYCHAIN_SERVICE} -a {KEYCHAIN_ACCOUNT} -w"
            )
            .into());
        }
        Ok(Notion { http: reqwest::Client::new(), token })
    }

    async fn get(&self, url: &str) -> Result<Value> {
        for _ in 0..5 {
            let resp = self
                .http
                .get(url)
                .bearer_auth(&self.token)
                .header("Notion-Version", VERSION)
                .send()
                .await?;
            let status = resp.status();
            if status.as_u16() == 429 {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            let body: Value = resp.json().await?;
            if !status.is_success() {
                let code = body["code"].as_str().unwrap_or("unknown");
                let hint = if code == "object_not_found" {
                    " (is the page shared with the HD EGM Workflow connection?)"
                } else {
                    ""
                };
                return Err(format!(
                    "Notion API {}: {code}: {}{hint}",
                    status.as_u16(),
                    body["message"].as_str().unwrap_or("")
                )
                .into());
            }
            return Ok(body);
        }
        Err("Notion API kept rate-limiting; try again later".into())
    }

    pub async fn page(&self, id: &str) -> Result<Page> {
        let v = self.get(&format!("{API}/pages/{}", compact_id(id))).await?;
        Ok(Page {
            id: v["id"].as_str().unwrap_or(id).to_string(),
            title: page_title(&v),
            last_edited_time: v["last_edited_time"].as_str().unwrap_or("").to_string(),
        })
    }

    /// Direct children of a block or page, following pagination.
    pub async fn children(&self, id: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = format!("{API}/blocks/{}/children?page_size=100", compact_id(id));
            if let Some(c) = &cursor {
                url.push_str("&start_cursor=");
                url.push_str(c);
            }
            let v = self.get(&url).await?;
            out.extend(v["results"].as_array().cloned().unwrap_or_default());
            match (v["has_more"].as_bool(), v["next_cursor"].as_str()) {
                (Some(true), Some(next)) => cursor = Some(next.to_string()),
                _ => return Ok(out),
            }
        }
    }

    /// The full block tree of a page. Child pages are not descended into:
    /// they are separate documents.
    pub async fn tree(&self, id: &str) -> Result<Vec<Block>> {
        let mut out = Vec::new();
        for value in self.children(id).await? {
            let kind = value["type"].as_str().unwrap_or("");
            let descend = value["has_children"].as_bool() == Some(true)
                && kind != "child_page"
                && kind != "child_database";
            let children = if descend {
                let child_id = value["id"].as_str().unwrap_or("").to_string();
                Box::pin(self.tree(&child_id)).await?
            } else {
                Vec::new()
            };
            out.push(Block { value, children });
        }
        Ok(out)
    }
}

pub fn compact_id(id: &str) -> String {
    id.chars().filter(|c| *c != '-').collect()
}

fn page_title(page: &Value) -> String {
    page["properties"]
        .as_object()
        .and_then(|props| props.values().find(|p| p["type"] == "title"))
        .and_then(|p| p["title"].as_array())
        .map(|parts| parts.iter().filter_map(|t| t["plain_text"].as_str()).collect())
        .unwrap_or_default()
}
