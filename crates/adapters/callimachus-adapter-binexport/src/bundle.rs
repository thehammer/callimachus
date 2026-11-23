//! Parsing of a binary-export bundle (format 1). See `docs/binary-export.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

pub const BUNDLE_FORMAT: u64 = 1;
pub const BUNDLE_KIND: &str = "binary-export";

#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    pub id: String,
    #[serde(default)]
    pub profile_sha256: Option<String>,
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub address_bits: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct Cartridge {
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub format: u64,
    pub kind: String,
    #[serde(default)]
    pub platform: Option<Platform>,
    #[serde(default)]
    cartridge: Option<Cartridge>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionRecord {
    pub address: String,
    pub name: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub c: Option<String>,
    #[serde(default)]
    pub asm: Option<String>,
    #[serde(default = "default_true")]
    pub decompile_ok: bool,
    #[serde(default)]
    pub discovered_by: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableRecord {
    pub address: String,
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub readers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortRecord {
    pub address: String,
    #[serde(default)]
    pub port: Option<String>,
    pub name: String,
    #[serde(default)]
    pub readers: Vec<String>,
    #[serde(default)]
    pub writers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RamRecord {
    pub address: String,
    pub name: String,
    #[serde(default)]
    pub readers: Vec<String>,
    #[serde(default)]
    pub writers: Vec<String>,
}

/// A fully parsed bundle: manifest plus all jsonl indexes.
#[derive(Debug)]
pub struct Bundle {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub functions: Vec<FunctionRecord>,
    pub tables: Vec<TableRecord>,
    pub ports: Vec<PortRecord>,
    pub ram: Vec<RamRecord>,
    by_address: HashMap<u64, usize>,
}

/// Parse `0x0071d0` (or bare hex) into a number.
pub fn parse_address(s: &str) -> Result<u64> {
    let t = s.trim();
    let digits = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    u64::from_str_radix(digits, 16).with_context(|| format!("invalid address {s:?}"))
}

/// Read and validate `manifest.json`.
pub fn read_manifest(root: &Path) -> Result<Manifest> {
    let path = root.join("manifest.json");
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {} (is this a binary-export bundle?)",
            path.display()
        )
    })?;
    let manifest: Manifest =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    if manifest.format != BUNDLE_FORMAT {
        bail!(
            "unsupported binary-export bundle format {} in {} (this build reads format {BUNDLE_FORMAT})",
            manifest.format,
            path.display()
        );
    }
    if manifest.kind != BUNDLE_KIND {
        bail!(
            "manifest kind is {:?}, expected {BUNDLE_KIND:?} ({})",
            manifest.kind,
            path.display()
        );
    }
    Ok(manifest)
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(root: &Path, file: &str) -> Result<Vec<T>> {
    let path = root.join(file);
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    raw.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l).with_context(|| format!("{}:{}", path.display(), i + 1))
        })
        .collect()
}

impl Bundle {
    pub fn load(root: &Path) -> Result<Self> {
        let manifest = read_manifest(root)?;
        let functions: Vec<FunctionRecord> = read_jsonl(root, "functions.jsonl")?;
        let mut by_address = HashMap::new();
        for (i, f) in functions.iter().enumerate() {
            by_address.insert(parse_address(&f.address)?, i);
        }
        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            functions,
            tables: read_jsonl(root, "tables.jsonl")?,
            ports: read_jsonl(root, "ports.jsonl")?,
            ram: read_jsonl(root, "ram.jsonl")?,
            by_address,
        })
    }

    pub fn function_at(&self, address: u64) -> Option<&FunctionRecord> {
        self.by_address.get(&address).map(|&i| &self.functions[i])
    }

    /// Read a bundle-relative text file named by a functions.jsonl record.
    pub fn read_text(&self, rel: &str) -> Result<String> {
        let path = self.root.join(rel);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }
}

/// True when `list` (hex address strings) contains `address`.
pub fn lists_address(list: &[String], address: u64) -> bool {
    list.iter().any(|a| parse_address(a).ok() == Some(address))
}

/// Merge the bundle's platform record into a corpus config object.
///
/// Sets `platform` and `cartridge_sha256` from the manifest, and
/// `address_mask` (derived from `address_bits`) unless already present.
/// `cpu_spaces` and other keys are left alone.
pub fn merge_platform_config(config: &mut Value, bundle: &Path) -> Result<()> {
    let manifest = read_manifest(bundle)?;
    if !config.is_object() {
        *config = json!({});
    }
    let obj = config.as_object_mut().expect("object ensured above");
    if let Some(p) = &manifest.platform {
        obj.insert(
            "platform".into(),
            json!({
                "id": p.id,
                "profile_sha256": p.profile_sha256,
                "cpu": p.cpu,
                "address_bits": p.address_bits,
            }),
        );
        if let Some(bits) = p.address_bits.filter(|b| (1..=64).contains(b)) {
            let mask = if bits == 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            };
            obj.entry("address_mask")
                .or_insert_with(|| Value::String(format!("0x{mask:x}")));
        }
    }
    if let Some(sha) = manifest.cartridge.and_then(|c| c.sha256) {
        obj.insert("cartridge_sha256".into(), Value::String(sha));
    }
    Ok(())
}
