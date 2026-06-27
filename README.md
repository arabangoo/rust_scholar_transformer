# rust_scholar_transformer

> **Rust 기반 실시간 멀티소스 리트리벌 코어 엔진**
>
> `arXiv · 뉴스 · 블로그 · 유튜브 · 웹` 을 동시에 가져와
> **정규화 → 중복제거 → 순위융합 → 캐싱** 한 뒤, LLM/Python 이 한 줄 import 로 쓰는 검색 코어.

이 문서는 라이브러리의 **완결된 개발자 매뉴얼**이다. 설계 철학, 공개 API, 소스별 동작과 한계,
Python 사용법, 새 소스 추가 방법, 빌드/테스트 절차를 모두 담는다.

[주요 참고 논문]

1. Reciprocal Rank Fusion outperforms Condorcet and individual Rank Learning Methods (Cormack et al., SIGIR 2009) - https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf
2. LSHBloom: Memory-efficient, Extreme-scale Document Deduplication (2024) - https://arxiv.org/abs/2411.04257
3. Precise Zero-Shot Dense Retrieval without Relevance Labels (HyDE, 2022) - https://arxiv.org/abs/2212.10496

---

## 목차

1. [핵심 특징](#1-핵심-특징)
2. [빠른 시작](#2-빠른-시작)
3. [설치와 Cargo Feature](#3-설치와-cargo-feature)
4. [아키텍처](#4-아키텍처)
5. [공통 데이터 모델 레퍼런스](#5-공통-데이터-모델-레퍼런스)
6. [공개 API 레퍼런스](#6-공개-api-레퍼런스)
7. [소스별 동작과 성숙도](#7-소스별-동작과-성숙도)
8. [중복 제거와 순위 융합](#8-중복-제거와-순위-융합)
9. [캐시와 Rate limit](#9-캐시와-rate-limit)
10. [Python 바인딩 (PyO3)](#10-python-바인딩-pyo3)
11. [서비스 파이프라인에 붙이기](#11-서비스-파이프라인에-붙이기)
12. [새 소스 어댑터 추가하기](#12-새-소스-어댑터-추가하기)
13. [빌드 · Feature 조합 · 테스트](#13-빌드--feature-조합--테스트)
14. [디렉토리 구조](#14-디렉토리-구조)
15. [라이선스](#15-라이선스)

---

## 1. 핵심 특징

RAG / 리서치 에이전트에서 외부 지식 retrieval(최신 논문·뉴스·웹)은 답변 품질을 직접 좌우하지만,
Rust 생태계에는 **여러 소스를 비동기로 통합·정규화·중복제거·재랭킹하는 단일 코어**가 비어 있다.
이 라이브러리는 단순 fetch 가 아니라 **"LLM 이 믿고 인용할 재료로 정리하는 수집·정규화·랭킹 엔진"** 을 지향한다.

| 원칙 | 의미 |
|---|---|
| **Multi-source federation** | 한 질의로 arXiv·뉴스·블로그·유튜브·웹을 동시(tokio fan-out)에 가져와 단일 [`Document`](#5-공통-데이터-모델-레퍼런스) 로 정규화. |
| **Deterministic local processing** | 라이브 API 결과는 비결정적이지만, **로컬 처리(정규화·중복제거·순위융합)는 결정적** — 같은 결과 집합은 항상 같은 순서·중복제거 결과. |
| **Plugin-extensible** | 새 소스는 [`Source`](#61-source-트레이트) 트레이트 하나만 구현하면 끝. 코어를 건드리지 않는다. |
| **Resilient** | 한 소스가 실패·timeout 해도 나머지 결과는 살리고 실패는 [`SearchReport`](#63-searchreport--에러-타입) 의 경고로 보고. |
| **Model-free, zero-FFI default** | 기본 빌드는 **pure Rust / 신경망·LLM 의존성 없음**. 의미 이해·요약·재랭크는 호출자(LLM)의 몫. |

### 왜 멀티소스 페더레이션인가

단발 fetch 는 네트워크 바운드라 언어 차이가 작다. Rust 가 차별화를 내는 곳은 **대량 동시 fan-out + 결과 후처리**(정규화·중복제거·순위융합·캐싱)다 — CPU·동시성 작업이라 GIL 없는 Rust 가 처리량·메모리에서 우위.

```text
한 LLM 의 fetch(url) = 한 페이지 가져오기
이 엔진 = 여러 소스를 동시에 가져와 → 정규화 → 중복제거 → 순위융합 → 인용 가능한 재료로 정리
```

코어와 호출자의 경계는 하나다 — **모델 없이 결정적으로 되는가(코어), 아니면 의미를 이해·생성해야 하는가(호출자).**
무엇을 검색할지·결과 요약·답변 생성·의미 재랭킹은 호출자(LLM/Python)가 맡는다.

---

## 2. 빠른 시작

### Rust 라이브러리

```rust
use rust_scholar_transformer::{ArxivOaiSource, Engine, GoogleNewsSource, SearchQuery};

#[tokio::main]
async fn main() {
    let mut engine = Engine::new();
    engine.register(Box::new(ArxivOaiSource::new()));    // 키 불필요
    engine.register(Box::new(GoogleNewsSource::new()));  // 키 불필요

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

결과는 JSON 문자열로 반환된다(직렬화 안정성 우선).

```python
import json
from rust_scholar_transformer import Retriever

r = Retriever(sources=["arxiv", "news"])          # 키 0개로 동작
report = json.loads(r.search("agentic loop engineering", limit=20))
for d in report["docs"]:
    print(d["source"], d["title"], d["url"])
```

---

## 3. 설치와 Cargo Feature

`Cargo.toml`:

```toml
[dependencies]
rust_scholar_transformer = "0.1"
```

### Feature 목록

| Feature | 활성화 대상 | 비고 |
|---|---|---|
| (기본) | 모든 소스 어댑터 + L1 인메모리 캐시 + rate limit | pure Rust, zero FFI |
| **`cache-disk`** | L2 디스크 캐시 | `redb`(순수 Rust, 만료 검사) |
| **`python`** | PyO3 cdylib 바인딩 | `pyo3`(abi3, Python 3.9 이상) |

```toml
# default = []   ← 무료·무키 코어 + 모든 어댑터, zero FFI
rust_scholar_transformer = { version = "0.1", features = ["cache-disk"] }
```

> default 빌드는 외부 .so/.dll 및 subprocess 를 요구하지 않는다. L1 캐시(`moka`)·rate limiter 는 순수 Rust 라 기본 포함이며, 디스크 캐시(`redb`)·Python 바인딩(`pyo3`)만 명시 feature 다. 전송 보안은 rustls(시스템 OpenSSL 의존 회피).

---

## 4. 아키텍처

```text
SearchQuery → Engine(fan-out + 소스별 rate limit) → [Source 어댑터들 동시 실행]
    → 정규화(단일 Document) → RRF 융합 + 정체성 병합 → MinHash-LSH 근접중복
    → recency/날짜 필터 → 캐시 → SearchReport(docs + warnings)
```

핵심은 **단일 Document 레이어**와 **Source 트레이트 추상화**다. 각 어댑터는 소스 고유 응답을 [`Document`](#5-공통-데이터-모델-레퍼런스) 로 정규화하고, 엔진은 소스 종류를 몰라도 융합·중복제거·필터를 수행한다.

- **수집** — 어댑터가 [`Source`](#61-source-트레이트) 구현체로 [`Engine`](#62-engine) 에 등록된다. 소스별 rate 정책이 있으면 등록 시 자동으로 limiter 가 붙는다.
- **융합** — 소스별 결과 순서를 보존해 [RRF](#8-중복-제거와-순위-융합) 로 융합하고, 같은 문서가 여러 소스에 등장하면 순위 기여가 합산된다.
- **중복제거** — 정체성(식별자) 기준 병합 + MinHash-LSH 근접중복 병합.
- **필터·캐시** — recency/날짜 필터 후 상위 결과를 캐시에 적재하고 반환한다.

라이브 외부 API 를 호출하므로 결과 자체는 비결정적이다. 결정성은 로컬 처리(정규화·중복제거·순위융합)에만 적용된다.

---

## 5. 공통 데이터 모델 레퍼런스

`model` 모듈. 모든 타입은 `serde::{Serialize, Deserialize}` 를 구현한다.

```rust
pub struct Document {
    pub identity:     DocIdentity,           // 중복제거 기준
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
    pub sources:      Vec<SourceKind>,       // 같은 문서를 제공한 소스들(병합 추적)
    pub score:        Score,
    pub extra:        HashMap<String, serde_json::Value>, // 소스별 원본 보존
}

pub struct DocIdentity {
    pub doi:           Option<String>,
    pub arxiv_id:      Option<String>,       // 정규화: 2301.00001v2 → 2301.00001
    pub canonical_url: Option<String>,       // 추적 파라미터 제거 후 정규 URL
    pub title_hash:    u64,                  // 식별자가 없을 때의 폴백(FNV-1a)
}

pub struct Author { pub name: String, pub id: Option<String> }

/// serde 직렬화 시 enum 이름 그대로.
pub enum SourceKind { Arxiv, News, Blog, Youtube, Web }

pub struct Score {
    pub fused:     f64,   // RRF 융합 결과 (1차 정렬축)
    pub freshness: f64,   // 신선도 (2차)
    pub authority: f64,   // 출처 신뢰도 (2차)
    pub relevance: f64,   // 질의 적합도 (보조)
}
```

질의 타입:

```rust
pub struct SearchQuery {
    pub text:            String,
    pub sources:         Vec<SourceKind>,   // 비면 등록된 전체 대상
    pub limit:           usize,             // 통합 top-K(소스별 fetch 수 아님)
    pub language:        Option<String>,
    pub recency:         Option<Recency>,   // Day | Week | Month | Year
    pub from_date:       Option<DateTime<Utc>>,
    pub to_date:         Option<DateTime<Utc>>,
    pub include_content: bool,
    pub expansions:      Vec<String>,       // 호출자가 제공하는 쿼리 확장(코어는 받아 처리만)
}

// 헬퍼: SearchQuery::from_text(text, limit) · .with_sources(vec![..])
```

---

## 6. 공개 API 레퍼런스

### 6.1 `Source` 트레이트

모든 소스 어댑터가 구현한다. 새 소스는 이것만 구현해 엔진에 등록한다.

```rust
#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn kind(&self) -> SourceKind;
    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError>;
    fn rate_policy(&self) -> RatePolicy;   // 기본 구현 제공
}

pub struct RatePolicy { pub min_interval_ms: u64, pub max_concurrency: usize, pub daily_quota: Option<u32> }
```

### 6.2 `Engine`

```rust
let mut engine = Engine::new()
    .with_timeout(Duration::from_secs(10))      // 소스별 timeout
    .with_near_dup_threshold(0.7);              // MinHash-LSH 병합 임계값
engine.register(Box::new(source));              // 소스 등록(rate limiter 자동 부착)
engine = engine.with_cache(Box::new(cache));    // 선택: 결과 캐시

let report: SearchReport = engine.search(query).await;
```

`search` 는 캐시 확인 → 동시 fan-out(rate limit 적용) → RRF 융합 → 정체성·근접 중복제거 → 날짜 필터 → 상위 `limit` → 캐시 적재 순으로 동작한다.

### 6.3 `SearchReport` · 에러 타입

```rust
pub struct SearchReport { pub docs: Vec<Document>, pub warnings: Vec<ConnectorWarning> }
pub struct ConnectorWarning { pub source: SourceKind, pub message: String }

#[derive(thiserror::Error)]
pub enum FetchError { Http(String), Parse(String), RateLimit(String), InvalidQuery(String) }
```

한 소스가 실패해도 전체 검색은 실패하지 않는다 — 성공 결과와 함께 실패가 `warnings` 로 보고된다.

### 6.4 어댑터 생성자

```rust
ArxivOaiSource::new()                       // OAI-PMH(1순위). .with_from_days(n) · .with_set("cs")
ArxivSource::new()                          // 라이브 API(보조). .with_retry(n, delay) · .with_user_agent(..)
RssSource::new(vec![FeedSource::new("name", "url").with_reliability(0.95)])
GoogleNewsSource::new()                     // .with_locale("ko", "KR", "KR:ko", "ko")
YoutubeSource::new(api_key)                 // Data API v3 키 필요
WebSource::new(Box::new(BraveProvider::new(api_key)))   // Brave 키 필요
```

### 6.5 캐시 · Rate limit

```rust
Cache (trait): async fn get(&str) -> Option<SearchReport>; async fn put(String, SearchReport);
MemoryCache::new(ttl, max_capacity)              // L1, 기본
DiskCache::open(path, ttl)                       // L2, feature = "cache-disk"
MinIntervalLimiter::from_millis(ms)              // 소스별 최소 간격(엔진이 자동 사용)
```

---

## 7. 소스별 동작과 성숙도

| 소스 | 타입 / 키 | 상태 |
|---|---|---|
| arXiv (1순위) | `ArxivOaiSource` / 키 불필요 | OAI-PMH 로 최근 N일 수확 + 메모리 키워드 필터. rate limit 노출 없음 |
| arXiv (보조) | `ArxivSource` / 키 불필요 | 라이브 검색 API. HTTP status 선확인 + 429 백오프. 공유 IP 차단 위험으로 보조 |
| 블로그/RSS | `RssSource` / 키 불필요 | 구독 피드 동시 fetch + 키워드 필터 + 피드별 신뢰도 |
| 뉴스 | `GoogleNewsSource` / 키 불필요 | Google News RSS 검색(공개 RSS, API 아님) |
| 유튜브 | `YoutubeSource` / **키 필요** | Data API v3 메타데이터. 자막은 다루지 않음(아래) |
| 웹 | `WebSource` + `BraveProvider` / **키 필요** | provider 추상화. 기본 Brave |

검증·한계 메모:

- **무료·무키 코어.** arXiv(OAI-PMH) + Google News RSS + 블로그 RSS 는 API 키 없이 동작한다. 유튜브·웹만 키가 필요하며(선택), 키 없이 등록하면 그 어댑터만 경고로 처리되고 나머지 결과는 정상 반환된다(우아한 부분 실패). LLM 을 호출하지 않으므로 LLM 키도 필요 없다.
- **arXiv 는 OAI-PMH 가 1순위.** 라이브 검색 API 는 공유 IP 에서 공손한 클라이언트도 지속적으로 HTTP 429(rate limit)로 차단된다. OAI-PMH(Open Archives Initiative Protocol for Metadata Harvesting) 미러는 rate limit 노출이 없어 안정적으로 최근 논문을 수확한다.
- **유튜브 자막은 다루지 않는다.** `captions.download` 는 영상 소유자 권한이 필요하고 비공식 추출은 이용약관 위반이다. 메타데이터(제목·채널·설명·게시일·URL)만 다룬다.
- **웹 provider 는 교체 가능.** 검색 공급망이 자주 바뀌고 대부분 유료라 `WebProvider` 트레이트 뒤에 둔다. 스크래핑 기반 provider 는 법적 리스크가 있어 자체 인덱스·합법 API provider 와 분리해 명시 사용한다.
- **의미 기반 재랭크는 코어 밖.** cross-encoder 등은 외부 모델이 필요하고 model-free 경계를 벗어나므로, 호출자(LLM/Python)가 맡거나 Semantic Scholar 가 제공하는 임베딩으로 우회한다.

---

## 8. 중복 제거와 순위 융합

`dedup` · `fusion` 모듈.

중복 제거는 두 단계다.

1. **정체성 기준(결정적):** DOI → arXiv ID 정규화 → canonical URL → 제목 정규화 해시 우선순위로 묶어 병합한다. 가장 풍부한 메타를 채택하고 `sources` 에 기여 소스를 누적, `extra` 에 원본을 보존한다.
2. **MinHash-LSH 근접중복:** 식별자가 달라 1단계가 놓친 near-dup(제목·요약이 미세하게 다른 preprint 와 게재본 등)을 제목+요약 shingle 의 MinHash 시그니처로 잡는다. LSH(Locality-Sensitive Hashing) 밴딩으로 후보만 추려 전수 비교를 피한다(64 해시 / 16 밴드, 임계값은 `with_near_dup_threshold` 로 조정).

순위 융합은 점수를 더하지 않는다(이종 소스의 점수는 스케일·의미가 비교 불가). **RRF(Reciprocal Rank Fusion)** 로 순위만 융합한다.

```text
fused(d) = Σ_sources  1 / (k + rank_source(d))      (k = 60)
```

- 점수 스케일에 무관·결정적. 같은 문서가 여러 소스에 등장하면 순위 기여가 합산되어 위로 올라간다.
- 2차 신호(타이브레이커): **신선도**(오늘 1.0 / 3일 0.8 / 7일 0.6 / 30일 0.3 / 그 이상 0.1)와 **출처 신뢰도**(arXiv 0.90 / 블로그 0.85 / 뉴스 0.80 / 웹 0.55 / 유튜브 0.45, 피드별 신뢰도 지정 시 그 값 우선).

---

## 9. 캐시와 Rate limit

캐시(선택, `Engine::with_cache`):

| 레벨 | 구현 | 성격 |
|---|---|---|
| L1 인메모리 | `MemoryCache`(moka, TTL + 용량 상한) | 동일 쿼리 반복 시 API 호출 없이 즉시 반환 |
| L2 디스크 | `DiskCache`(redb, `cache-disk` feature, 만료 검사) | 프로세스 재시작 후에도 유지 |

캐시 키 = 정규화 쿼리 텍스트 + 정렬된 소스 종류 + limit + 날짜 조건. 소스 조합이 달라지면 자동 캐시 미스.

Rate limit: 소스의 `rate_policy()` 에 최소 간격이 있으면 엔진이 등록 시 `MinIntervalLimiter` 를 자동으로 달고, 소스 호출 전에 간격을 강제한다(예: arXiv OAI 0.5초, arXiv 라이브 3초). 소스별 독립이라 fan-out 시 서로 간섭하지 않는다.

---

## 10. Python 바인딩 (PyO3)

### 설치

```bash
# PyPI 게시 후 — Rust 툴체인 불필요, abi3 휠을 그대로 받는다
pip install rust_scholar_transformer

# 소스에서(최신 main / 게시 전) — 설치 머신에 Rust 툴체인 + maturin 필요
pip install maturin
maturin develop --features python
```

### API

```python
import json
from rust_scholar_transformer import Retriever

# sources: "arxiv" | "news" | "blog" | "youtube" | "web" (기본 ["arxiv","news"])
r = Retriever(
    sources=["arxiv", "news", "blog", "youtube", "web"],
    rss_feeds=["https://aws.amazon.com/blogs/machine-learning/feed/"],
    youtube_api_key="...",   # 선택
    brave_api_key="...",     # 선택
)
report = json.loads(r.search("multi-agent RAG", limit=20))   # JSON 문자열 반환
```

동기 우선(sync-first) 설계 — 내부 tokio 런타임에서 완료시켜 일반 함수처럼 노출한다(asyncio/Jupyter 환경 차이 회피). async awaitable 변형은 후속.

### 빌드 · 게시

abi3 휠(Python 3.9 이상 단일 휠)로 빌드한다. 전 플랫폼 휠 빌드·PyPI 게시는 `.github/workflows/release.yml`(`PyO3/maturin-action`)이 `v*` 태그 push 시 수행한다(Trusted Publishing, API 토큰 불필요).

---

## 11. 서비스 파이프라인에 붙이기

### 11.1 Python LLM 파이프라인 — 검색 코어로

```python
import json
from rust_scholar_transformer import Retriever

retriever = Retriever(sources=["arxiv", "news"])

def answer(question: str, llm) -> str:
    docs = json.loads(retriever.search(question, limit=12))["docs"]
    context = "\n\n".join(f"[{i+1}] {d['title']}\n{d.get('summary','')}\n{d['url']}"
                          for i, d in enumerate(docs))
    return llm.generate(question, context=context)   # 의미 이해·답변은 호출자 LLM
```

### 11.2 Rust 서비스에 임베드

```rust
let mut engine = Engine::new();
engine.register(Box::new(ArxivOaiSource::new()));
engine.register(Box::new(GoogleNewsSource::new()));
let report = engine.search(SearchQuery::from_text(&q, 20)).await;
// report.docs 를 그대로 직렬화해 응답하거나 후단 LLM 컨텍스트로 전달
```

### 11.3 키 주입

API 키는 코드에 하드코딩하지 않고 런타임에 주입한다 — Rust 는 생성자(`YoutubeSource::new(env_key)`), Python 은 `Retriever(...)` 인자. 환경변수·시크릿 매니저에서 읽어 넘긴다.

---

## 12. 새 소스 어댑터 추가하기

[`Source`](#61-source-트레이트) 트레이트만 구현하면 코어를 건드리지 않고 등록된다.

```rust
use rust_scholar_transformer::{Document, FetchError, RatePolicy, SearchQuery, Source, SourceKind};

pub struct MySource { /* client, base_url, ... */ }

#[async_trait::async_trait]
impl Source for MySource {
    fn kind(&self) -> SourceKind { SourceKind::Web }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<Document>, FetchError> {
        // 1) fetch  2) 파싱  3) Document 로 정규화(canonical_url·title_hash·published_at 채우기)
        Ok(vec![])
    }

    fn rate_policy(&self) -> RatePolicy {
        RatePolicy { min_interval_ms: 0, max_concurrency: 2, daily_quota: None }
    }
}

// engine.register(Box::new(MySource { .. }));
```

웹 검색 provider 를 추가할 때는 [`WebProvider`](#64-어댑터-생성자) 트레이트를 구현해 `WebSource` 에 끼우면 `Web` 소스 종류를 공유하면서 provider 만 교체된다.

---

## 13. 빌드 · Feature 조합 · 테스트

```bash
# 기본: 무료·무키 코어 + 모든 어댑터, zero FFI
cargo build

# 디스크 캐시 / Python 바인딩
cargo build --features cache-disk
cargo build --features python

# 테스트 / 린트
cargo test
cargo clippy --all-targets
```

통합 테스트는 wiremock 으로 HTTP 응답을 모킹해 결정적으로 검증한다(라이브 API 의존 회피).

---

## 14. 디렉토리 구조

- `Cargo.toml`
- `pyproject.toml` — maturin (`python` feature)
- `src/`
  - `lib.rs` — 크레이트 루트, re-export
  - `model.rs` — Document / Author / DocIdentity / SearchQuery / Score
  - `error.rs` — FetchError / SearchReport / ConnectorWarning
  - `source.rs` — Source 트레이트 + RatePolicy
  - `engine.rs` — fan-out + rate limit + 융합 + 중복제거 + 날짜 필터 + 캐시 조립
  - `normalize.rs` — URL 정규화 · 날짜 파싱 · HTML 정제 · 해시
  - `dedup.rs` — 정체성 + MinHash-LSH 근접중복
  - `fusion.rs` — RRF 순위 융합 + 신선도 · 신뢰도
  - `cache.rs` — L1(moka) + L2(redb, 선택)
  - `ratelimit.rs` — 소스별 최소 간격 제한기
  - `python.rs` — PyO3 바인딩(`python` feature)
  - `sources/` — arxiv · arxiv_oai · rss · news · youtube · web · feed_common
- `tests/` — 모킹 HTTP 기반 결정적 통합 테스트
- `.github/workflows/release.yml` — 전 플랫폼 abi3 휠 빌드 → PyPI 게시

---

## 15. 라이선스

Apache-2.0
