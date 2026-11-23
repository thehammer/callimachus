//! Fact records (format 1): confirmed knowledge imported from outside the index.

use serde::{Deserialize, Serialize};

pub const CONFIDENCE_VALUES: [&str; 3] = ["confirmed", "inferred", "unknown"];

/// Numeric confidence for a fact's `confidence` label:
/// `confirmed` → 1.0, `inferred` → 0.6, anything else (`unknown`) → 0.3.
pub fn confidence_score(label: &str) -> f32 {
    match label {
        "confirmed" => 1.0,
        "inferred" => 0.6,
        _ => 0.3,
    }
}

/// One confirmed (or inferred) fact. Stored as the payload of a
/// `CorrectionKind::Fact` scholion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactRecord {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub topic: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub data: serde_json::Value,
    pub confidence: String,
    pub evidence: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// True when `id` matches `[a-z0-9][a-z0-9._-]{0,95}`.
pub fn is_valid_fact_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    id.len() <= 96
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

impl FactRecord {
    /// Validate a raw JSON record. The error string is the rejection reason.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        let obj = value.as_object().ok_or("record is not an object")?;
        let text = |key: &str| -> Result<Option<String>, String> {
            match obj.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(format!("`{key}` must be a string")),
            }
        };
        let id = text("id")?.ok_or("missing `id`")?;
        if !is_valid_fact_id(&id) {
            return Err(format!("invalid id `{id}`"));
        }
        let kind = text("kind")?.ok_or("missing `kind`")?;
        if kind.is_empty() {
            return Err("empty `kind`".into());
        }
        let summary = text("summary")?.ok_or("missing `summary`")?;
        let n = summary.chars().count();
        if !(1..=300).contains(&n) {
            return Err(format!("`summary` must be 1-300 chars (got {n})"));
        }
        let confidence = text("confidence")?.ok_or("missing `confidence`")?;
        if !CONFIDENCE_VALUES.contains(&confidence.as_str()) {
            return Err(format!(
                "`confidence` must be confirmed|inferred|unknown (got `{confidence}`)"
            ));
        }
        let evidence = match obj.get("evidence") {
            None => return Err("missing `evidence`".into()),
            Some(serde_json::Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(
                        item.as_str()
                            .ok_or("`evidence` must be a list of strings")?
                            .to_string(),
                    );
                }
                out
            }
            Some(_) => return Err("`evidence` must be a list of strings".into()),
        };
        if evidence.is_empty() {
            return Err("`evidence` must not be empty".into());
        }
        let record = FactRecord {
            id,
            kind,
            topic: text("topic")?,
            summary,
            data: match obj.get("data") {
                None | Some(serde_json::Value::Null) => serde_json::json!({}),
                Some(v) => v.clone(),
            },
            confidence,
            evidence,
            notes: text("notes")?,
        };
        record.validate_data()?;
        Ok(record)
    }

    /// The `data.space` string, if present.
    pub fn space(&self) -> Option<&str> {
        self.data.get("space").and_then(|v| v.as_str())
    }

    /// The `data.address` string, if present.
    pub fn address(&self) -> Option<&str> {
        self.data.get("address").and_then(|v| v.as_str())
    }

    fn validate_data(&self) -> Result<(), String> {
        use crate::corrections::address::parse_address;
        let need_str = |key: &str| -> Result<&str, String> {
            self.data
                .get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("`data.{key}` is required for kind `{}`", self.kind))
        };
        let need_addr = |key: &str| -> Result<(), String> {
            let s = need_str(key)?;
            parse_address(s)
                .map(|_| ())
                .ok_or_else(|| format!("`data.{key}` is not a hex address: `{s}`"))
        };
        match self.kind.as_str() {
            "variable" | "table" | "routine" => {
                need_str("space")?;
                need_addr("address")
            }
            "region" => {
                need_str("space")?;
                need_addr("start")?;
                need_addr("end")?;
                let start = parse_address(need_str("start")?);
                let end = parse_address(need_str("end")?);
                if let (Some(start), Some(end)) = (start, end)
                    && end < start
                {
                    return Err("`data.end` must not be less than `data.start`".into());
                }
                Ok(())
            }
            "format" | "rule" | "constant" | "behaviour" | "asset" => {
                if self.data.is_object() {
                    Ok(())
                } else {
                    Err(format!("`data` must be an object for kind `{}`", self.kind))
                }
            }
            "variant" => {
                need_str("variant")?;
                match self.data.get("affects") {
                    None => Ok(()),
                    Some(serde_json::Value::Array(items))
                        if items.iter().all(|i| i.is_string()) =>
                    {
                        Ok(())
                    }
                    Some(_) => Err("`data.affects` must be a list of strings".into()),
                }
            }
            "platform" => {
                if self.data.is_object() {
                    Ok(())
                } else {
                    Err("`data` must be an object for kind `platform`".into())
                }
            }
            _ => Ok(()),
        }
    }
}
