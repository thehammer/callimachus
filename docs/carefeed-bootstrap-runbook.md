# Carefeed Production Pinakes Bootstrap Runbook

**Audience:** Hammer (operator). This is a step-by-step checklist for building the production
pinakes for knowledge.carefeed.com. Every command is literal. Run these in order.

**What this does:** Creates `carefeed-production.pinakes` containing six indexed code corpora
(`admin-portal`, `referral-monitor`, `payments`, `family-portal`, `employee-app`, `carefeed-core`),
plus a `carefeed` collection containing all six for Alex's cross-repo queries.

**Estimated time:** 3–6 hours active indexing wall-clock (API), after the cost checkpoint.

---

## 1. Prerequisites

### 1.1 `calli` binary

The `calli` binary must be on PATH.

```bash
which calli          # should print e.g. ~/.cargo/bin/calli
calli --version      # should print calli 0.1.0 (or newer)
```

If not found, build from the callimachus repo:

```bash
cd ~/Code/callimachus
cargo install --path crates/callimachus-cli
```

### 1.2 LLM provider configuration

Callimachus uses an LLM for extraction, summarization, and contract/purpose passes.
The `index` command reads its provider from the config file:

```
~/Library/Application Support/callimachus/config.toml
```

**Option A — Anthropic API (recommended for bootstrap; ~50–100 chunks/min)**

Set this in `config.toml`:

```toml
[llm]
provider = "anthropic"
api_key = "sk-ant-api..."    # or omit and set ANTHROPIC_API_KEY env var

[model_tiers]
enabled = true
default = "sonnet"
haiku_model = "claude-haiku-4-5"
sonnet_model = "claude-sonnet-4-5"
opus_model = "claude-opus-4-7"
```

Cost note: Using Anthropic API directly is ~5× faster than claude-code subprocess and
costs real money (Claude Sonnet tokens). See the cost sampling checkpoint in §2 before
committing to the full run.

**Option B — claude-code subprocess (~10–20 chunks/min, covered by subscription)**

```toml
[llm]
provider = "claude-code"
```

Requires `claude` on PATH. Slower but subscription-covered. Not recommended for the
full bootstrap (6× slower means 18–36 hours vs 3–6 hours).

**Verify provider is configured:**

```bash
calli config show
```

You should see your `[llm]` provider and masked api_key.

### 1.3 Fresh `git pull` on all seven checkouts

Run before indexing to avoid stale-commit detection issues:

```bash
git -C ~/Code/admin-portal pull
git -C ~/Code/referral-monitor pull
git -C ~/Code/payments pull
git -C ~/Code/family-portal pull
git -C ~/Code/employee_app pull
git -C ~/Code/carefeed-core pull
git -C ~/Code/core-packages pull
```

### 1.4 Carefeed-core combined parent directory

The `carefeed-core` corpus combines two repos (`carefeed-core` + `core-packages`)
by pointing the corpus source at a parent directory containing symlinks to both.
Build it once:

```bash
mkdir -p ~/Code/carefeed-core-combined
ln -sfn ~/Code/carefeed-core ~/Code/carefeed-core-combined/carefeed-core
ln -sfn ~/Code/core-packages ~/Code/carefeed-core-combined/core-packages

# Verify:
ls -la ~/Code/carefeed-core-combined/
# Should show two symlinks: carefeed-core -> ... and core-packages -> ...
```

**Note on git detection:** Callimachus detects git history per-repo; the parent
directory has no git history of its own. Both repos fall back to v1-tree change
detection (all files processed on each run). This is expected and acceptable —
W20's incremental reindex will handle subsequent runs correctly.

---

## 2. Cost sampling checkpoint

**Index referral-monitor first** (smallest code corpus). Check the cost before
committing to all six.

```bash
# Create the production pinakes by copying your working index.
# (This takes ~30 seconds for a 1.8 GB file.)
cp ~/Library/Application\ Support/callimachus/index.pinakes \
   /tmp/carefeed-production.pinakes

# Index referral-monitor into the production pinakes.
# referral-monitor is already in the baseline — this is a dry-run to verify setup.
CALLIMACHUS_PINAKES=/tmp/carefeed-production.pinakes \
  calli corpus status referral-monitor
```

If `referral-monitor` shows as `ready` with a reasonable entity count, the baseline
copy worked correctly. The two baseline corpora (`admin-portal`, `referral-monitor`)
do NOT need re-indexing.

**Estimate cost for the four new corpora:**

```bash
# Dry-run index for payments (one of the four new corpora) to count chunks.
# No LLM calls, no writes.
CALLIMACHUS_PINAKES=/tmp/carefeed-production.pinakes \
  calli corpus add code "Payments" ~/Code/payments --id payments

CALLIMACHUS_PINAKES=/tmp/carefeed-production.pinakes \
  calli index payments --dry-run
```

The dry-run output shows `Chunks: N`. With the Anthropic API:

- Each chunk goes through up to 9 passes; LLM is involved in ~5 of them.
- Rough cost: **$0.50–$2.00 per 1,000 chunks** (Haiku for simple, Sonnet for complex).
- Multiply by the sum of chunk counts across the four new corpora.

**Decision point:** If the cost estimate is acceptable, proceed to §3.
Otherwise, switch to `claude-code` provider (§1.2 Option B) for subscription-covered
but slower indexing.

---

## 3. Full run

Use the bootstrap driver. It is idempotent — re-running after a failure resumes
from where it left off (already-indexed corpora are skipped).

```bash
cd ~/Code/callimachus

./scripts/bootstrap-carefeed-pinakes.sh \
  --pinakes /tmp/carefeed-production.pinakes \
  --manifest scripts/carefeed-corpora.manifest.json \
  --concurrency 8
```

**What the script does per corpus:**
1. Checks if the corpus is already registered; skips `corpus add` if so.
2. For `baseline: true` corpora (admin-portal, referral-monitor): skips `index`.
3. For `baseline: false` corpora: runs `calli index <id> --concurrency 8`.
4. Creates the `carefeed` collection if it doesn't exist.
5. Adds each corpus as a collection member (idempotent — safe to repeat).

**Expected per-corpus durations (API provider, 8 concurrency):**

| Corpus | Est. duration |
|---|---|
| admin-portal | — (baseline, skip) |
| referral-monitor | — (baseline, skip) |
| payments | 30–90 min |
| family-portal | 30–90 min |
| employee-app | 30–90 min |
| carefeed-core | 60–180 min |

**Resumability:** If indexing is interrupted, re-run the same command.
The `index` pass is idempotent — already-processed chunks and entities are skipped.

**To index a single corpus manually** (if you prefer to run one at a time):

```bash
export CALLIMACHUS_PINAKES=/tmp/carefeed-production.pinakes

# Register (skip if already registered):
calli corpus add code "Payments" ~/Code/payments --id payments

# Index:
calli index payments --concurrency 8

# Verify:
calli corpus status payments
```

---

## 4. Curation (fast-follow — not required for launch)

> **Not yet applicable at launch.** This section applies when Confluence corpora
> are added (fast-follow). The manifest's `_fast_follow_corpora` section shows the
> intended Jira/Confluence entries.

When ready:

1. Edit `scripts/carefeed-corpora.manifest.json`.
2. Move the desired entry from `_fast_follow_corpora` into `corpora`.
3. Fill in `api_token_env` and `space_keys`.
4. **Confluence curation gate:** only include spaces Hammer has approved.
   Sensitive spaces (HR, Legal, Finance) should never enter the index.
   Edit `space_keys` before running — this is the only content gate.
5. Re-run `bootstrap-carefeed-pinakes.sh`. Already-indexed corpora are skipped.

**Required env vars for fast-follow:**
- `JIRA_API_TOKEN` — Atlassian API token (Settings → Security → API tokens)
- `CONFLUENCE_API_TOKEN` — same token works for Confluence

---

## 5. Verification

Run the verify script after the bootstrap completes:

```bash
./scripts/verify-pinakes.sh \
  --pinakes /tmp/carefeed-production.pinakes \
  --manifest scripts/carefeed-corpora.manifest.json
```

### Expected output (all passes):

```
── Check 1: All launch corpora present and ready ─────
  ✓ admin-portal — ready
  ✓ referral-monitor — ready
  ✓ payments — ready
  ✓ family-portal — ready
  ✓ employee-app — ready
  ✓ carefeed-core — ready

── Check 2: Entities > 0 per corpus (search proxy) ───
  ✓ admin-portal — 3842 entities
  ...

── Check 3: Chunks > 0 per corpus ────────────────────
  ✓ admin-portal — 12104 chunks
  ...

── Check 4: 'carefeed' collection has all launch corpora
  ✓ Collection 'carefeed' exists
  ✓   member: admin-portal
  ...

── Check 5: Line spans present ───────────────────────
  ✓ 6 launch corpora have entities with line spans

── Check 6: Cross-corpus collection proxy ────────────
  ✓ 6 launch corpora have entities — collection_search would return cross-corpus results

══════════════════════════════════════════════════════
  Results: 29/29 checks passed
  Status:  ALL CHECKS PASSED
```

### If a check fails:

| Failure | Resolution |
|---|---|
| `NOT FOUND in corpus list` | Corpus wasn't registered. Re-run bootstrap. |
| `status is 'registered'` | Indexing didn't run. Re-run bootstrap; check LLM config. |
| `0 entities` | Index ran but extracted nothing. Check `calli corpus status <id>` for failed runs. |
| `NOT in collection` | Re-run bootstrap; the collection add-member step is idempotent. |
| Line span fail | Only 0–1 corpora have line spans. May indicate the code adapter didn't run the structure pass. Re-index with `calli index <id> --pass structure`. |

---

## 6. Handoff: upload to S3

Upload the completed pinakes as generation 0 to the S3 location defined by W20's infra.

```bash
# Placeholder — replace BUCKET and PREFIX with W20's values when available.
aws s3 cp /tmp/carefeed-production.pinakes \
  s3://<BUCKET>/<PREFIX>/generation-0/carefeed-production.pinakes

# Verify the upload:
aws s3 ls s3://<BUCKET>/<PREFIX>/generation-0/
```

W20 will update this runbook with the actual bucket path when the infra is provisioned.
Until then, keep the local file at `/tmp/carefeed-production.pinakes` as the authoritative
copy.

---

## Appendix A: Pinakes file management

The production pinakes is a SQLite file. Keep backups before major operations:

```bash
# Backup before each index session:
cp /tmp/carefeed-production.pinakes \
   /tmp/carefeed-production.pinakes.bak-$(date +%Y%m%d)

# Check file size and integrity:
ls -lh /tmp/carefeed-production.pinakes
sqlite3 /tmp/carefeed-production.pinakes "PRAGMA integrity_check;"
```

**Note on the 25+ non-launch corpora in the baseline:** The working index contains
corpora for internal tools (mother, bishop, teri, nostromo, core-is*, etc.). These
are copied into the production pinakes but are NOT members of the `carefeed` collection
and are not exposed by the launch app's corpus allowlist. Physical removal of these
corpora is deferred — a `calli corpus remove` CLI command does not yet exist; when it
does, run a cleanup pass to reduce file size.

---

## Appendix B: Troubleshooting

**`Error: corpus already exists`**
The corpus was registered in a previous run. This is unexpected — the bootstrap
script checks first. If you see this manually, it means the corpus ID exists. Use
`calli corpus status <id>` to inspect it.

**`adapter not yet available for corpus kind 'jira'`**
The Jira adapter is a fast-follow. Only `code` kind is supported at launch.

**`could not detect an LLM provider`**
The `index` command couldn't find an LLM. Check `calli config show` and ensure
`[llm] provider` and `api_key` are set in `config.toml`, or set `ANTHROPIC_API_KEY`
in the environment.

**Index runs very slowly (< 5 chunks/min)**
You may be hitting rate limits. Reduce `--concurrency` to 4, or switch to the
`claude-code` provider. Check the calli log output for rate-limit messages.

**Indexing stops mid-pass**
Re-run the bootstrap. The pipeline is idempotent — already-processed items are
skipped with `status: skipped`. Check `calli corpus status <id>` for run history.
