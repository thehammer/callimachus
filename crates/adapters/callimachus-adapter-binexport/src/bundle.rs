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
    /// Row counts the exporter claims per jsonl file (`functions`, `tables`,
    /// `ports`, `ram`). Absent keys are not cross-checked.
    #[serde(default)]
    pub counts: HashMap<String, u64>,
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
    /// Reader address → indexes into `tables`, ascending.
    tables_by_reader: HashMap<u64, Vec<usize>>,
    /// Function address → `(index into ports, reads, writes)`, ascending.
    ports_by_user: HashMap<u64, Vec<(usize, bool, bool)>>,
    /// Function address → `(index into ram, reads, writes)`, ascending.
    ram_by_user: HashMap<u64, Vec<(usize, bool, bool)>>,
}

/// How one function touches a port or RAM record.
#[derive(Debug, Clone, Copy)]
pub struct Access<'a, T> {
    pub record: &'a T,
    pub reads: bool,
    pub writes: bool,
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

/// Read one jsonl file. `functions.jsonl` is `required`; the others may be
/// absent only when the manifest's count for them is 0 or missing. A present
/// file must match the manifest count when one is given.
fn read_jsonl<T: for<'de> Deserialize<'de>>(
    root: &Path,
    file: &str,
    manifest: &Manifest,
    count_key: &str,
    required: bool,
) -> Result<Vec<T>> {
    let path = root.join(file);
    let expected = manifest.counts.get(count_key).copied();
    if !path.exists() {
        if required {
            bail!(
                "{} is missing (required by the bundle format)",
                path.display()
            );
        }
        return match expected {
            None | Some(0) => Ok(vec![]),
            Some(n) => bail!(
                "{} is missing but the manifest counts {n} {count_key}",
                path.display()
            ),
        };
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut rows = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: invalid record", path.display(), i + 1))?;
        rows.push(row);
    }
    if let Some(n) = expected
        && n != rows.len() as u64
    {
        bail!(
            "{}: manifest counts {n} {count_key} but {} rows were read",
            path.display(),
            rows.len()
        );
    }
    Ok(rows)
}

fn parse_addresses(list: &[String], file: &str) -> Result<Vec<u64>> {
    list.iter()
        .map(|a| parse_address(a).with_context(|| format!("in a reader/writer list of {file}")))
        .collect()
}

impl Bundle {
    pub fn load(root: &Path) -> Result<Self> {
        let manifest = read_manifest(root)?;
        let functions: Vec<FunctionRecord> =
            read_jsonl(root, "functions.jsonl", &manifest, "functions", true)?;
        let tables: Vec<TableRecord> =
            read_jsonl(root, "tables.jsonl", &manifest, "tables", false)?;
        let ports: Vec<PortRecord> = read_jsonl(root, "ports.jsonl", &manifest, "ports", false)?;
        let ram: Vec<RamRecord> = read_jsonl(root, "ram.jsonl", &manifest, "ram", false)?;

        let mut by_address = HashMap::new();
        for (i, f) in functions.iter().enumerate() {
            let a = parse_address(&f.address)?;
            if let Some(prev) = by_address.insert(a, i) {
                bail!(
                    "duplicate function address 0x{a:x} in functions.jsonl ({} and {})",
                    functions[prev].name,
                    f.name
                );
            }
        }

        let mut tables_by_reader: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, t) in tables.iter().enumerate() {
            let mut readers = parse_addresses(&t.readers, "tables.jsonl")?;
            readers.sort_unstable();
            readers.dedup();
            for a in readers {
                tables_by_reader.entry(a).or_default().push(i);
            }
        }
        let ports_by_user = index_accesses(
            ports.iter().map(|p| (&p.readers, &p.writers)),
            "ports.jsonl",
        )?;
        let ram_by_user =
            index_accesses(ram.iter().map(|r| (&r.readers, &r.writers)), "ram.jsonl")?;

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            functions,
            tables,
            ports,
            ram,
            by_address,
            tables_by_reader,
            ports_by_user,
            ram_by_user,
        })
    }

    pub fn function_at(&self, address: u64) -> Option<&FunctionRecord> {
        self.by_address.get(&address).map(|&i| &self.functions[i])
    }

    /// Tables whose `readers` list the function at `address`.
    pub fn tables_read_by(&self, address: u64) -> impl Iterator<Item = &TableRecord> {
        self.tables_by_reader
            .get(&address)
            .into_iter()
            .flatten()
            .map(|&i| &self.tables[i])
    }

    /// Ports the function at `address` reads or writes.
    pub fn ports_used_by(&self, address: u64) -> impl Iterator<Item = Access<'_, PortRecord>> {
        accesses(&self.ports_by_user, &self.ports, address)
    }

    /// RAM records the function at `address` reads or writes.
    pub fn ram_used_by(&self, address: u64) -> impl Iterator<Item = Access<'_, RamRecord>> {
        accesses(&self.ram_by_user, &self.ram, address)
    }

    /// Read a bundle-relative text file named by a functions.jsonl record.
    /// The path must stay inside the bundle: absolute paths, `..` and symlinks
    /// that leave the root are refused. `Ok(None)` when the file does not exist.
    pub fn read_text(&self, rel: &str) -> Result<Option<String>> {
        let rel_path = Path::new(rel);
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            bail!("bundle path escapes the bundle root: {rel}");
        }
        let root = self.root.canonicalize()?;
        let path = match root.join(rel_path).canonicalize() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("resolving {rel} in {}", self.root.display()));
            }
        };
        if !path.starts_with(&root) {
            bail!("bundle path escapes the bundle root: {rel}");
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(Some(text))
    }
}

type AccessIndex = HashMap<u64, Vec<(usize, bool, bool)>>;

fn index_accesses<'a>(
    rows: impl Iterator<Item = (&'a Vec<String>, &'a Vec<String>)>,
    file: &str,
) -> Result<AccessIndex> {
    let mut out: AccessIndex = HashMap::new();
    for (i, (readers, writers)) in rows.enumerate() {
        let mut flags: HashMap<u64, (bool, bool)> = HashMap::new();
        for a in parse_addresses(readers, file)? {
            flags.entry(a).or_default().0 = true;
        }
        for a in parse_addresses(writers, file)? {
            flags.entry(a).or_default().1 = true;
        }
        for (a, (r, w)) in flags {
            out.entry(a).or_default().push((i, r, w));
        }
    }
    Ok(out)
}

fn accesses<'a, T>(
    index: &'a AccessIndex,
    records: &'a [T],
    address: u64,
) -> impl Iterator<Item = Access<'a, T>> {
    index
        .get(&address)
        .into_iter()
        .flatten()
        .map(|&(i, reads, writes)| Access {
            record: &records[i],
            reads,
            writes,
        })
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
