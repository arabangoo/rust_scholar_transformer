# rust_scholar_transformer

> **A real-time multi-source retrieval core engine, written in Rust.**
>
> It fetches `arXiv · news · blogs · YouTube · web` concurrently, then runs
> **normalize -> deduplicate -> rank fusion -> caching**, and exposes a search core that an LLM or Python uses with a one-line import.

This document is the **complete developer manual** for the library. It covers the design philosophy, the public API, per-source behavior and limitations, Python usage, how to add a new source, and the build and test workflow.

**Key references**

1. Reciprocal Rank Fusion outperforms Condorcet and individual Rank Learning Methods (Cormack et al., SIGIR 2009) - https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf
2. LSHBloom: Memory-efficient, Extreme-scale Document Deduplication (2024) - https://arxiv.org/abs/2411.04257
3. Precise Zero-Shot Dense Retrieval without Relevance Labels (HyDE, 2022) - https://arxiv.org/abs/2212.10496

---

## Table of Contents

1. [Key Features](#1-key-features)
2. [Quick Start](#2-quick-start)
3. [Installation and Cargo Features](#3-installation-and-cargo-features)
4. [Architecture](#4-architecture)
5. [Common Data Model Reference](#5-common-data-model-reference)
6. [Public API Reference](#6-public-api-reference)
7. [Per-Source Behavior and Maturity](#7-per-source-behavior-and-maturity)
8. [Deduplication and Rank Fusion](#8-deduplication-and-rank-fusion)
9. [Cache and Rate Limiting](#9-cache-and-rate-limiting)
10. [Python Bindings (PyO3)](#10-python-bindings-pyo3)
11. [Embedding into a Service Pipeline](#11-embedding-into-a-service-pipeline)
12. [Adding a New Source Adapter](#12-adding-a-new-source-adapter)
13. [Build, Feature Combinations, and Testing](#13-build-feature-combinations-and-testing)
14. [Directory Layout](#14-directory-layout)
15. [License](#15-license)

---

## 1. Key Features

In a RAG / research agent, external knowledge retrieval (the latest papers, news, and web) directly determines answer quality, yet the Rust ecosystem lacks **a single core that federates, normalizes, deduplicates, and re-ranks multiple sources asynchronously**. This library is not a plain fetch; it aims to be **a collection, normalization, and ranking engine that prepares material an LLM can trust and cite**.

| Principle | What it means |
|---|---|
| **Multi-source federation** | One query fetches arXiv, news, blogs, YouTube, and web concurrently (tokio fan-out) and normalizes them into a single [`Document`](#5-common-data-model-reference). |
| **Deterministic local processing** | Live API results are non-deterministic, but **the local processing (normalization, deduplication, rank fusion) is deterministic**: the same result set always yields the same order and dedup outcome. |
| **Plugin-extensible** | A new source needs only one trait, [`Source`](#61-the-source-trait). The core stays untouched. |
| **Resilient** | If one source fails or times out, the rest of the results survive and the failure is reported as a warning in the [`SearchReport`](#63-searchreport-and-error-types). |
| **Model-free, zero-FFI by default** | The default build is **pure Rust with no neural-network or LLM dependency**. Semantic understanding, summarization, and re-ranking are the caller's (LLM's) job. |

### Why multi-source federation

A single fetch is network-bound, so the language difference is small. Where Rust makes a difference is **large concurrent fan-out plus result post-processing** (normalization, deduplication, rank fusion, caching): these are CPU and concurrency work, where GIL-free Rust wins on throughput and memory.

```text
one LLM's fetch(url) = fetch a single page
this engine = fetch many sources at once -> normalize -> deduplicate -> rank fusion -> prepare citable material
```

There is one boundary between the core and the caller: **can it be done deterministically without a model (core), or does it require understanding and generating meaning (caller).** What to search, summarizing results, generating answers, and semantic re-ranking are the caller's job (LLM/Python).

---

## 2. Quick Start

### Rust library

```rust
use rust_scholar_transformer::{ArxivOaiSource, Engine, GoogleNewsSource, SearchQuery};

#[tokio::main]
async fn main() {
    let mut engine = Engine::new();
    engine.register(Box::new(ArxivOaiSource::new()));    // no key needed
    engine.register(Box::new(GoogleNewsSource::new()));  // no key needed

    let report = engine.search(SearchQuery::from_text("agentic loop engineering", 20)).await;
    for d in &report.docs {
        println!("[{:?}] {} ({})", d.source, d.title, d.url);
    }
    for w in &report.warnings {
        eprintln!("warning from {:?}: {}", w.source, w.message);
    }
}
```

### Python

Results are returned as a JSON string (serialization stability first).

```python
import json
from rust_scholar_transformer import Retriever

r = Retriever(sources=["arxiv", "news"])          # works with zero keys
report = json.loads(r.search("agentic loop engineering", limit=20))
for d in report["docs"]:
    print(d["source"], d["title"], d["url"])
```

---

## 3. Installation and Cargo Features

`Cargo.toml`:

```toml
[dependencies]
rust_scholar_transformer = "0.1"
```

### Feature list

| Feature | Enables | Notes |
|---|---|---|
| (default) | all source adapters + L1 in-memory cache + rate limiting | pure Rust, zero FFI |
| **`cache-disk`** | L2 disk cache | `redb` (pure Rust, expiry checks) |
| **`python`** | PyO3 cdylib bindings | `pyo3` (abi3, Python 3.9+) |

```toml
# default = []  # free, keyless core + all adapters, zero FFI
rust_scholar_transformer = { version = "0.1", features = ["cache-disk"] }
```

> The default build requires no external `.so/.dll` and no subprocess. The L1 cache (`moka`) and rate limiter are pure Rust and included by default; only the disk cache (`redb`) and Python bindings (`pyo3`) are explicit features. Transport security uses rustls (avoiding a system OpenSSL dependency).

---

## 4. Architecture

```text
SearchQuery -> Engine (fan-out + per-source rate limit) -> [Source adapters run concurrently]
    -> normalize (single Document) -> RRF fusion + identity merge -> MinHash-LSH near-duplicate
    -> recency/date filter -> cache -> SearchReport (docs + warnings)
```

The heart of it is the **single Document layer** and the **Source trait abstraction**. Each adapter normalizes a source's own response into a [`Document`](#5-common-data-model-reference), and the engine performs fusion, deduplication, and filtering without knowing the source kind.

- **Collection**: an adapter is registered in the [`Engine`](#62-engine) as a [`Source`](#61-the-source-trait) implementation. If a per-source rate policy exists, a limiter is attached automatically at registration.
- **Fusion**: per-source result order is preserved and fused with [RRF](#8-deduplication-and-rank-fusion); when the same document appears in multiple sources, its rank contributions add up.
- **Deduplication**: identity-based (identifier) merging plus MinHash-LSH near-duplicate merging.
- **Filter and cache**: after a recency/date filter, top results are loaded into the cache and returned.

Because it calls live external APIs, the results themselves are non-deterministic. Determinism applies only to the local processing (normalization, deduplication, rank fusion).

---

## 5. Common Data Model Reference

The `model` module. Every type implements `serde::{Serialize, Deserialize}`.

```rust
pub struct Document {
    pub identity:     DocIdentity,           // deduplication key
    pub source:       SourceKind,            // Arxiv | News | Blog | Youtube | Web
    pub title:        String,
    pub url:          String,
    pub authors:      Vec<Author>,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at:   DateTime<Utc>,
    pub summary:      Option<String>,
    pub content:      Option<String>,
    pub language:     Option<String>,
    pub tags:         Vec<String>,
    pub sources:      Vec<SourceKind>,       // sources that provided the same document (merge tracking)
    pub score:        Score,
    pub extra:        HashMap<String, serde_json::Value>, // per-source raw data preserved
}

pub struct DocIdentity {
    pub doi:           Option<String>,
    pub arxiv_id:      Option<String>,       // normalized: 2301.00001v2 -> 2301.00001
    pub canonical_url: Option<String>,       // canonical URL after removing tracking parameters
    pub title_hash:    u64,                  // fallback when no identifier (FNV-1a)
}

pub struct Author { pub name: String, pub id: Option<String> }

/// Serialized as the enum name by serde.
pub enum SourceKind { Arxiv, News, Blog, Youtube, Web }

pub struct Score {
    pub fused:     f64,   // RRF fusion result (primary sort axis)
    pub freshness: f64,   // freshness (secondary)
    pub authority: f64,   // source authority (secondary)
    pub relevance: f64,   // query relevance (auxiliary)
}
```

Query type:

```rust
pub struct SearchQuery {
    pub text:            String,
    pub sources:         Vec<SourceKind>,   // empty = all registered sources
    pub limit:           usize,             // unified top-K (not the per-source fetch count)
    pub language:        Option<String>,
    pub recency:         Option<Recency>,   // Day | Week | Month | Year
    pub from_date:       Option<DateTime<Utc>>,
    pub to_date:         Option<DateTime<Utc>>,
    pub include_content: bool,
    pub expansions:      Vec<String>,       // query expansions provided by the caller (the core only consumes them)
}

// helpers: SearchQuery::from_text(text, limit) . .with_sources(vec![..])
```

---

## 6. Public API Reference

### 6.1 The `Source` Trait

Implemented by every source adapter. A new source implements only this and registers with the engine.

```rust
#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn kind(&self) -> SourceKind;
    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError>;
    fn rate_policy(&self) -> RatePolicy;   // default implementation provided
}

pub struct RatePolicy { pub min_interval_ms: u64, pub max_concurrency: usize, pub daily_quota: Option<u32> }
```

### 6.2 `Engine`

```rust
let mut engine = Engine::new()
    .with_timeout(Duration::from_secs(10))      // per-source timeout
    .with_near_dup_threshold(0.7);              // MinHash-LSH merge threshold
engine.register(Box::new(source));              // register a source (rate limiter attached automatically)
engine = engine.with_cache(Box::new(cache));    // optional: result cache

let report: SearchReport = engine.search(query).await;
```

`search` runs in this order: check cache, concurrent fan-out (rate-limited), RRF fusion, identity and near-duplicate dedup, date filter, top `limit`, load into cache.

### 6.3 `SearchReport` and Error Types

```rust
pub struct SearchReport { pub docs: Vec<Document>, pub warnings: Vec<ConnectorWarning> }
pub struct ConnectorWarning { pub source: SourceKind, pub message: String }

#[derive(thiserror::Error)]
pub enum FetchError { Http(String), Parse(String), RateLimit(String), InvalidQuery(String) }
```

A single source failing does not fail the whole search: the failure is reported in `warnings` alongside the successful results.

### 6.4 Adapter Constructors

```rust
ArxivOaiSource::new()                       // OAI-PMH (primary). .with_from_days(n) . .with_set("cs")
ArxivSource::new()                          // live API (fallback). .with_retry(n, delay) . .with_user_agent(..)
RssSource::new(vec![FeedSource::new("name", "url").with_reliability(0.95)])
GoogleNewsSource::new()                     // .with_locale("ko", "KR", "KR:ko", "ko")
YoutubeSource::new(api_key)                 // requires a Data API v3 key
WebSource::new(Box::new(BraveProvider::new(api_key)))   // requires a Brave key
```

### 6.5 Cache and Rate Limiting

```rust
Cache (trait): async fn get(&str) -> Option<SearchReport>; async fn put(String, SearchReport);
MemoryCache::new(ttl, max_capacity)              // L1, default
DiskCache::open(path, ttl)                       // L2, feature = "cache-disk"
MinIntervalLimiter::from_millis(ms)              // per-source minimum interval (used automatically by the engine)
```

---

## 7. Per-Source Behavior and Maturity

| Source | Type / key | Status |
|---|---|---|
| arXiv (primary) | `ArxivOaiSource` / no key | harvests the last N days via OAI-PMH + in-memory keyword filter. No rate-limit exposure |
| arXiv (fallback) | `ArxivSource` / no key | live search API. Pre-checks HTTP status + 429 backoff. A fallback due to shared-IP blocking risk |
| Blogs/RSS | `RssSource` / no key | concurrent fetch of subscribed feeds + keyword filter + per-feed reliability |
| News | `GoogleNewsSource` / no key | Google News RSS search (public RSS, not an API) |
| YouTube | `YoutubeSource` / **key required** | Data API v3 metadata. Does not handle captions (below) |
| Web | `WebSource` + `BraveProvider` / **key required** | provider abstraction. Brave by default |

Verification and limitation notes:

- **Free, keyless core.** arXiv (OAI-PMH) + Google News RSS + blog RSS work without an API key. Only YouTube and web need a key (optional); if you register them without a key, only that adapter is handled as a warning and the rest of the results return normally (graceful partial failure). Since no LLM is called, no LLM key is needed either.
- **arXiv OAI-PMH is primary.** The live search API blocks even a polite client with persistent HTTP 429 (rate limit) from shared IPs. The OAI-PMH (Open Archives Initiative Protocol for Metadata Harvesting) mirror has no rate-limit exposure and reliably harvests recent papers.
- **YouTube captions are not handled.** `captions.download` requires the video owner's permission, and unofficial extraction violates the terms of service. Only metadata (title, channel, description, publish date, URL) is handled.
- **The web provider is replaceable.** The search supply chain changes often and is mostly paid, so it sits behind the `WebProvider` trait. Scraping-based providers carry legal risk, so they are kept separate from self-indexed or legal-API providers and used explicitly.
- **Semantic re-ranking is outside the core.** Cross-encoders and the like require an external model and cross the model-free boundary, so the caller (LLM/Python) handles it, or you route around it with embeddings from Semantic Scholar.

---

## 8. Deduplication and Rank Fusion

The `dedup` and `fusion` modules.

Deduplication is two stages.

1. **Identity-based (deterministic):** merge by the priority DOI -> arXiv ID normalization -> canonical URL -> title-normalized hash. The richest metadata is adopted, contributing sources accumulate in `sources`, and the originals are preserved in `extra`.
2. **MinHash-LSH near-duplicate:** catch near-dups that stage 1 missed because their identifiers differ (a preprint and its published version whose titles/summaries differ slightly) using MinHash signatures of title+summary shingles. LSH (Locality-Sensitive Hashing) banding narrows to candidates only, avoiding all-pairs comparison (64 hashes / 16 bands; the threshold is tuned with `with_near_dup_threshold`).

Rank fusion does not add scores (scores from heterogeneous sources have incomparable scales and meanings). It fuses ranks only with **RRF (Reciprocal Rank Fusion)**.

```text
fused(d) = Σ_sources  1 / (k + rank_source(d))      (k = 60)
```

- Independent of score scale, and deterministic. When the same document appears in multiple sources, its rank contributions add up and push it higher.
- Secondary signals (tie-breakers): **freshness** (today 1.0 / 3 days 0.8 / 7 days 0.6 / 30 days 0.3 / older 0.1) and **source authority** (arXiv 0.90 / blog 0.85 / news 0.80 / web 0.55 / YouTube 0.45; a per-feed reliability, when specified, takes precedence).

---

## 9. Cache and Rate Limiting

Cache (optional, `Engine::with_cache`):

| Level | Implementation | Character |
|---|---|---|
| L1 in-memory | `MemoryCache` (moka, TTL + capacity cap) | returns instantly with no API call on repeated identical queries |
| L2 disk | `DiskCache` (redb, `cache-disk` feature, expiry checks) | persists across process restarts |

Cache key = normalized query text + sorted source kinds + limit + date conditions. A different source combination automatically misses the cache.

Rate limiting: if a source's `rate_policy()` has a minimum interval, the engine attaches a `MinIntervalLimiter` at registration and enforces the interval before calling the source (for example, arXiv OAI 0.5s, arXiv live 3s). Each source is independent, so they do not interfere during fan-out.

---

## 10. Python Bindings (PyO3)

### Installation

```bash
# After PyPI publication: no Rust toolchain needed, grab the abi3 wheel
pip install rust_scholar_transformer

# From source (latest main / before publication): requires a Rust toolchain + maturin on the install machine
pip install maturin
maturin develop --features python
```

### API

```python
import json
from rust_scholar_transformer import Retriever

# sources: "arxiv" | "news" | "blog" | "youtube" | "web" (default ["arxiv","news"])
r = Retriever(
    sources=["arxiv", "news", "blog", "youtube", "web"],
    rss_feeds=["https://aws.amazon.com/blogs/machine-learning/feed/"],
    youtube_api_key="...",   # optional
    brave_api_key="...",     # optional
)
report = json.loads(r.search("multi-agent RAG", limit=20))   # returns a JSON string
```

Sync-first design: the work is completed on an internal tokio runtime and exposed like an ordinary function (avoiding asyncio/Jupyter environment differences). An async awaitable variant is future work.

### Build and publish

Built as an abi3 wheel (a single wheel for Python 3.9+). All-platform wheel builds and PyPI publishing are done by `.github/workflows/release.yml` (`PyO3/maturin-action`) on a `v*` tag push (Trusted Publishing, no API token needed).

---

## 11. Embedding into a Service Pipeline

### 11.1 Python LLM pipeline: as a search core

```python
import json
from rust_scholar_transformer import Retriever

retriever = Retriever(sources=["arxiv", "news"])

def answer(question: str, llm) -> str:
    docs = json.loads(retriever.search(question, limit=12))["docs"]
    context = "\n\n".join(f"[{i+1}] {d['title']}\n{d.get('summary','')}\n{d['url']}"
                          for i, d in enumerate(docs))
    return llm.generate(question, context=context)   # understanding and answering is the caller's LLM
```

### 11.2 Embed into a Rust service

```rust
let mut engine = Engine::new();
engine.register(Box::new(ArxivOaiSource::new()));
engine.register(Box::new(GoogleNewsSource::new()));
let report = engine.search(SearchQuery::from_text(&q, 20)).await;
// serialize report.docs directly as the response, or pass it as downstream LLM context
```

### 11.3 Key injection

Do not hardcode API keys; inject them at runtime. In Rust via the constructor (`YoutubeSource::new(env_key)`), in Python via the `Retriever(...)` arguments. Read them from environment variables or a secret manager and pass them in.

---

## 12. Adding a New Source Adapter

Implement only the [`Source`](#61-the-source-trait) trait and it registers without touching the core.

```rust
use rust_scholar_transformer::{Document, FetchError, RatePolicy, SearchQuery, Source, SourceKind};

pub struct MySource { /* client, base_url, ... */ }

#[async_trait::async_trait]
impl Source for MySource {
    fn kind(&self) -> SourceKind { SourceKind::Web }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        // 1) fetch  2) parse  3) normalize into Document (fill canonical_url, title_hash, published_at)
        Ok(vec![])
    }

    fn rate_policy(&self) -> RatePolicy {
        RatePolicy { min_interval_ms: 0, max_concurrency: 2, daily_quota: None }
    }
}

// engine.register(Box::new(MySource { .. }));
```

To add a web search provider, implement the [`WebProvider`](#64-adapter-constructors) trait and plug it into `WebSource`; it shares the `Web` source kind while swapping only the provider.

---

## 13. Build, Feature Combinations, and Testing

```bash
# Default: free, keyless core + all adapters, zero FFI
cargo build

# Disk cache / Python bindings
cargo build --features cache-disk
cargo build --features python

# Test / lint
cargo test
cargo clippy --all-targets
```

Integration tests mock HTTP responses with wiremock and verify deterministically (avoiding a live API dependency).

---

## 14. Directory Layout

- `Cargo.toml`
- `pyproject.toml` (maturin, `python` feature)
- `src/`
  - `lib.rs` (crate root, re-exports)
  - `model.rs` (Document / Author / DocIdentity / SearchQuery / Score)
  - `error.rs` (FetchError / SearchReport / ConnectorWarning)
  - `source.rs` (Source trait + RatePolicy)
  - `engine.rs` (fan-out + rate limit + fusion + dedup + date filter + cache assembly)
  - `normalize.rs` (URL normalization, date parsing, HTML cleanup, hashing)
  - `dedup.rs` (identity + MinHash-LSH near-duplicate)
  - `fusion.rs` (RRF rank fusion + freshness, authority)
  - `cache.rs` (L1 moka + L2 redb, optional)
  - `ratelimit.rs` (per-source minimum-interval limiter)
  - `python.rs` (PyO3 bindings, `python` feature)
  - `sources/` (arxiv, arxiv_oai, rss, news, youtube, web, feed_common)
- `tests/` (deterministic integration tests over mocked HTTP)
- `.github/workflows/release.yml` (all-platform abi3 wheel build -> PyPI publish)

---

## 15. License

Apache-2.0
