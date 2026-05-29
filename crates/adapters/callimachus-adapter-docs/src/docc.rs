/// Typed structs for the DocC JSON format.
///
/// Uses `#[serde(default)]` throughout — the DocC schema varies significantly
/// across symbol kinds and Apple occasionally omits optional fields.
/// `primaryContentSections[].content` is left as `serde_json::Value` because
/// the content node tree is deeply variant and we only need to render it
/// heuristically (matching the v1 Python renderer contract).
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level DocC JSON page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccPage {
    pub metadata: DoccMetadata,
    pub abstract_text: Vec<DoccInlineNode>,
    pub primary_content_sections: Vec<DoccPrimarySection>,
    pub topic_sections: Vec<DoccTopicSection>,
    pub relationships: Vec<DoccRelationship>,
    pub references: HashMap<String, DoccReference>,
    /// Platform availability (top-level field in some DocC versions).
    pub availability: Vec<DoccAvailability>,
}

impl DoccPage {
    /// Parse a DocC JSON page from a raw `serde_json::Value`.
    pub fn from_value(v: &serde_json::Value) -> Self {
        // Custom parse: `abstract` is a reserved word so serde can't use the field name directly.
        let metadata: DoccMetadata = v
            .get("metadata")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let abstract_text: Vec<DoccInlineNode> = v
            .get("abstract")
            .and_then(|a| serde_json::from_value(a.clone()).ok())
            .unwrap_or_default();

        let primary_content_sections: Vec<DoccPrimarySection> = v
            .get("primaryContentSections")
            .and_then(|s| serde_json::from_value(s.clone()).ok())
            .unwrap_or_default();

        let topic_sections: Vec<DoccTopicSection> = v
            .get("topicSections")
            .and_then(|s| serde_json::from_value(s.clone()).ok())
            .unwrap_or_default();

        let relationships: Vec<DoccRelationship> = v
            .get("relationships")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or_default();

        let references: HashMap<String, DoccReference> = v
            .get("references")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or_default();

        let availability: Vec<DoccAvailability> = v
            .get("availability")
            .and_then(|a| serde_json::from_value(a.clone()).ok())
            .unwrap_or_default();

        DoccPage {
            metadata,
            abstract_text,
            primary_content_sections,
            topic_sections,
            relationships,
            references,
            availability,
        }
    }
}

/// Page metadata block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccMetadata {
    pub title: String,
    /// Symbol kind string, e.g. "class", "instm", "instp", "protocol".
    pub symbol_kind: String,
    /// Role heading, e.g. "Instance Method", "Class", "Protocol".
    pub role_heading: String,
    /// Modules this symbol belongs to.
    pub modules: Vec<DoccModule>,
    /// Platform availability embedded in metadata (alternate location).
    pub platforms: Vec<DoccPlatformAvailability>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccModule {
    pub name: String,
}

/// Platform availability as embedded in `metadata.platforms[]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccPlatformAvailability {
    pub name: String,
    pub introduced: String,
    pub deprecated: String,
    pub beta: bool,
}

/// A primary content section — declarations or prose content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccPrimarySection {
    pub kind: String,
    /// For `kind == "declarations"`: the declarations.
    pub declarations: Vec<DoccDeclaration>,
    /// For `kind == "content"`: the content node tree (kept as Value).
    pub content: serde_json::Value,
}

/// A Swift declaration block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccDeclaration {
    pub languages: Vec<String>,
    pub platforms: Vec<String>,
    pub tokens: Vec<DoccToken>,
}

/// A single token in a Swift declaration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccToken {
    pub kind: String,
    pub text: String,
    /// For `kind == "typeIdentifier"`: the DocC identifier.
    pub identifier: Option<String>,
    pub precise_identifier: Option<String>,
}

/// A topic section grouping child symbol identifiers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccTopicSection {
    pub title: String,
    pub identifiers: Vec<String>,
}

/// A relationship edge in the DocC relationships array.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccRelationship {
    /// e.g. "inheritsFrom", "conformsTo"
    #[serde(rename = "type")]
    pub kind: String,
    pub source: String,
    pub target: String,
}

/// Top-level availability entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccAvailability {
    pub name: String,
    pub introduced: Option<String>,
    pub deprecated: Option<String>,
}

/// A reference entry in the `references` map.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccReference {
    pub identifier: String,
    pub title: String,
    pub role: String,
    pub kind: String,
    pub url: String,
    #[serde(rename = "abstract")]
    pub abstract_text: Vec<DoccInlineNode>,
    pub path_components: Vec<String>,
    pub fragments: Vec<DoccToken>,
}

/// An inline content node (text, codeVoice, reference, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DoccInlineNode {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    pub identifier: String,
    pub code: String,
}

impl DoccInlineNode {
    /// Render this inline node to plain text.
    pub fn to_text(&self, references: &HashMap<String, DoccReference>) -> String {
        match self.kind.as_str() {
            "text" => self.text.clone(),
            "codeVoice" => format!("`{}`", self.code),
            "reference" => {
                if let Some(r) = references.get(&self.identifier) {
                    r.title.clone()
                } else {
                    self.identifier.rsplit('/').next().unwrap_or("").to_string()
                }
            }
            _ => self.text.clone(),
        }
    }
}

/// Derive a macOS availability string from a DocC page.
///
/// Checks `metadata.platforms[]` first, then falls back to `availability[]`.
pub fn macos_availability(page: &DoccPage) -> Option<(Option<String>, Option<String>)> {
    // Check metadata.platforms first (newer DocC format).
    for p in &page.metadata.platforms {
        if p.name.to_lowercase().contains("macos") {
            let introduced = if p.introduced.is_empty() {
                None
            } else {
                Some(p.introduced.clone())
            };
            let deprecated = if p.deprecated.is_empty() {
                None
            } else {
                Some(p.deprecated.clone())
            };
            if introduced.is_some() || deprecated.is_some() {
                return Some((introduced, deprecated));
            }
        }
    }
    // Fall back to top-level availability[].
    for a in &page.availability {
        if a.name.to_lowercase().contains("macos")
            && (a.introduced.is_some() || a.deprecated.is_some())
        {
            return Some((a.introduced.clone(), a.deprecated.clone()));
        }
    }
    None
}
