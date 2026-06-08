# rust_scholar_transformer

> **멀티소스 비동기 리서치 오케스트레이터 (Rust)**
>
> arXiv · Semantic Scholar · OpenAlex 학술 검색 + 웹서치 provider 어댑터를
> **동시 fan-out + 중복제거 + 재랭킹 + 캐싱**으로 통합한다.
> **PyO3** 로 어떤 Python 파이프라인에든 한 줄 import.

이 문서는 라이브러리의 **설계 문서**다. 설계 철학, 아키텍처, 공통 데이터 모델, 소스 어댑터 명세,
PyO3 바인딩, 캐싱·재랭킹·rate-limit 전략, 실제 난점, 개발 로드맵을 담는다.

> **자매 프로젝트와의 관계** — [`rust_markdown_transformer`](../rust_markdown_transformer) 가 *내 문서*를
> ingestion(파싱→Markdown)하는 입력단이라면, `rust_scholar_transformer` 는 *외부 논문·웹*을 retrieval 하는
> 검색단이다. 둘은 RAG / 리서치 스택의 양 끝을 이루며 같은 정체성(pure Rust · zero-FFI · Apache-2.0 · PyO3)을 공유한다.

---

## 목차

1. [핵심 특징](#1-핵심-특징)
2. [왜 만드는가](#2-왜-만드는가)
3. [아키텍처](#3-아키텍처)
4. [공통 데이터 모델](#4-공통-데이터-모델)
5. [SourceAdapter 트레이트](#5-sourceadapter-트레이트)
6. [Fan-out 엔진](#6-fan-out-엔진)
7. [중복 제거](#7-중복-제거)
8. [융합 + 재랭킹](#8-융합--재랭킹)
9. [캐싱](#9-캐싱)
10. [Rate limit 관리](#10-rate-limit-관리)
11. [소스 어댑터 명세](#11-소스-어댑터-명세)
12. [PyO3 바인딩](#12-pyo3-바인딩)
13. [Cargo Feature](#13-cargo-feature)
14. [빌딩블록(의존성)](#14-빌딩블록의존성)
15. [주의해야 할 실제 난점](#15-주의해야-할-실제-난점)
16. [개발 로드맵](#16-개발-로드맵)
17. [디렉토리 구조](#17-디렉토리-구조)
18. [라이선스](#18-라이선스)

---

## 1. 핵심 특징

| 원칙 | 의미 |
|---|---|
| **Async-resilient** | tokio fan-out 으로 여러 소스를 동시 질의하고, 한 소스가 느리거나 실패(rate limit·503)해도 나머지 결과는 수집한다. 소스별 독립 timeout. |
| **Academic-first, web-pluggable** | arXiv·Semantic Scholar·OpenAlex(무료·공개 API)는 1급 구현체로 내장. 웹 검색은 `SourceAdapter` 를 외부에서 구현하는 pluggable 어댑터(유료 provider라 opt-in). 정직하고 강한 경계. |
| **Identity-based dedup + RRF 융합** | 같은 논문이 여러 소스에 중복 인덱싱되므로 **DOI → arXiv ID → 제목 해시 → MinHash-LSH 근접중복** 으로 정체성 기반 병합하고, 이종 소스의 랭킹은 점수가 아니라 **RRF(Reciprocal Rank Fusion, 순위 융합)** 로 합친다. |
| **Pure Rust, zero-FFI (default)** | reqwest + tokio + serde + quick-xml/scraper. 기본 빌드는 네이티브 FFI·subprocess 없음. 단일 정적 바이너리·Apache-2.0. |
| **PyO3 한 줄 import** | `import rust_scholar_transformer` → 어떤 Python(LangChain·LlamaIndex·자체 에이전트)에든 검색 코어를 한 줄로 삽입. |

> **결정성에 대한 정직한 단서** — 이 라이브러리는 **라이브 외부 API**를 호출하므로 결과 자체는 결정적이지 않다
> (API 상태·rate limit·인덱스 변동에 의존). 결정성 원칙은 **로컬 처리**(정규화·중복제거·재랭킹)에만 적용된다 —
> 같은 입력 결과 집합은 항상 같은 순서·중복제거 결과를 낸다.

---

## 2. 왜 만드는가

RAG / 리서치 에이전트에서 **외부 지식 retrieval**(최신 논문·웹)은 LLM 답변 품질을 직접 좌우하지만,
Rust 생태계에는 **여러 학술 소스를 비동기로 통합·정규화·중복제거하는 단일 코어**가 비어 있다.

- **Rust 가 가장 큰 차별화를 내는 영역** — 단발 fetch 는 네트워크 바운드라 언어 차이가 작지만,
  **대량 동시 fan-out + 결과 후처리(정규화·dedup·재랭킹·캐싱)**는 CPU·동시성 작업이라 Rust(tokio, 저오버헤드,
  GIL 없음)가 Python 대비 처리량·메모리에서 우위. 에이전트가 폭발적으로 검색할 때 차이가 난다.
- **학술 3소스가 전부 무료·공개 API** — arXiv·Semantic Scholar·OpenAlex. 상업적 제약 없이 배포 가능(Apache-2.0).
- **웹 검색은 상류(provider)에 엔진이 있다** — 무료 공개 웹 인덱스는 없으므로, 일반 웹 검색은 유료 provider
  (Brave/Serper/Tavily) 또는 자체호스트(SearXNG)를 **pluggable 어댑터**로 감싼다. "검색 엔진을 만드는 게 아니라
  오케스트레이션한다"는 경계를 명확히 한다.

---

## 3. 아키텍처

```text
Python (AI orchestration · workflow · 에이전트)
    | PyO3 바인딩 (한 줄 import)
    v
Rust OrchestratorCore
    - SourceAdapter (trait)   : arXiv / SemanticScholar / OpenAlex / WebProvider(외부 구현)
    - FanoutEngine            : tokio::JoinSet 기반 동시 요청 + 소스별 timeout
    - NormalizationLayer      : 소스별 스키마 → 공통 Paper 구조체
    - DeduplicationEngine     : DOI / arXiv ID / 제목 해시 / MinHash-LSH 근접중복
    - FusionRerankEngine      : RRF(순위 융합) + citation/recency → (opt-in) cross-encoder 정밀 재랭크
    - CacheLayer              : L1 in-memory(moka) + L2 optional disk(sled/sqlite)
    - RateLimiter             : 소스별 독립 token bucket(governor)
```

흐름: `search(query)` → 각 SourceAdapter 동시 fan-out → 원본 결과 정규화 → 중복 제거 → 재랭킹 → 캐시 적재 → 통합 결과 반환.

핵심 설계점: **`SourceAdapter` 를 trait 으로 추상화**한다. 학술 소스는 구현체를 직접 제공하고, 웹 검색은 외부에서
trait 을 구현해 끼우는 pluggable 구조 → 코어를 건드리지 않고 소스 추가가 O(1).

---

## 4. 공통 데이터 모델

> ⚠️ **이 라이브러리의 최대 리스크는 기술이 아니라 정규화 스키마 설계다.** 소스마다 저자·식별자·메타 표현이 다르다.
> 핵심 필드는 강타입으로, 소스별 원본 메타는 `extra` 로 보존하는 **유연한 공통 구조체**를 초기에 잘 잡아야 이후가 순탄하다.

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Paper {
    pub identity:     PaperIdentity,        // dedup 기준 (DOI/arXiv ID/제목 해시)
    pub title:        String,
    pub authors:      Vec<Author>,
    pub abstract_:    Option<String>,
    pub year:         Option<u16>,
    pub venue:        Option<String>,
    pub citation_count: Option<u64>,
    pub url:          Option<String>,        // 대표 링크 (landing page / PDF)
    pub pdf_url:      Option<String>,
    pub sources:      Vec<&'static str>,     // 이 논문을 제공한 소스들 (병합 추적)
    pub score:        f64,                    // 재랭킹 점수
    pub extra:        std::collections::HashMap<String, serde_json::Value>, // 소스별 원본 보존
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Author { pub name: String, pub id: Option<String> }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaperIdentity {
    pub doi:       Option<String>,
    pub arxiv_id:  Option<String>,  // 정규화: 2301.00001v2 → 2301.00001
    pub title_hash: u64,            // 소문자화 + 특수문자 제거 후 ahash
}
```

각 어댑터가 내는 `RawResult` 는 소스 고유 형태이고, `NormalizationLayer` 가 이를 `Paper` 로 변환한다.
저자 표현 차이 예: arXiv `<author><name>` (XML) / Semantic Scholar `{"authors":[{"name":...}]}` /
OpenAlex `{"authorships":[{"author":{"display_name":...}}]}` → 모두 `Vec<Author>` 로 정규화.

---

## 5. SourceAdapter 트레이트

```rust
#[async_trait::async_trait]
pub trait SourceAdapter: Send + Sync {
    /// 소스 식별자 (예: "arxiv").
    fn source_id(&self) -> &'static str;

    /// 질의 → 소스 고유 원본 결과. 네트워크/파싱 에러는 Result 로.
    async fn search(&self, query: &SearchQuery) -> Result<Vec<RawResult>, AdapterError>;

    /// 소스별 rate-limit 정책 (오케스트레이터가 RateLimiter 구성에 사용).
    fn rate_policy(&self) -> RatePolicy;
}
```

서드파티/웹 provider 는 이 trait 만 구현해 `Orchestrator::register(adapter)` 로 끼운다. 코어 불변.

---

## 6. Fan-out 엔진

`tokio::JoinSet` 으로 소스별 독립 timeout 을 두고 동시 요청한다. 하나가 느리거나 실패해도 전체가 블록되지 않는다.

```rust
let mut set = tokio::task::JoinSet::new();
for adapter in self.adapters.iter().cloned() {
    let query = query.clone();
    set.spawn(async move {
        let id = adapter.source_id();
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            adapter.search(&query),
        ).await;
        (id, r)
    });
}
let mut raw = Vec::new();
while let Some(joined) = set.join_next().await {
    match joined {
        Ok((id, Ok(Ok(results)))) => raw.extend(tag(id, results)),
        Ok((id, _)) => log_partial_failure(id), // timeout/에러는 부분 실패로 기록, 계속 진행
        Err(_join_err) => {}
    }
}
```

학술 API 는 rate limit·간헐적 503 이 잦으므로 이 **부분 실패 허용(resilience)** 이 실용적으로 매우 중요하다.

---

## 7. 중복 제거

arXiv 논문이 Semantic Scholar·OpenAlex 에도 인덱싱된 경우가 흔하다. **논문 정체성** 기준 우선순위:

1. **DOI** — 가장 신뢰도 높음.
2. **arXiv ID 정규화** — `2301.00001v2` → `2301.00001`.
3. **제목 정규화 후 exact 해시** — 소문자화 + 특수문자 제거 후 `ahash`.
4. **MinHash-LSH 근접중복** — preprint↔게재본처럼 제목/초록이 미세하게 다른 경우를 잡는다. 제목+초록 shingle 의
   MinHash 시그니처를 LSH 버킷에 넣어 **Jaccard 유사 후보만 비교**(전수 비교 회피). 근접중복 탐지의 사실상 표준
   (MinHash/SimHash + LSH)이며, 대규모로 커지면 LSHBloom 기법으로 메모리를 절감한다. 순수 Rust.

중복으로 판정된 논문은 **병합**한다: 가장 풍부한 메타를 채택하고 `sources` 에 기여 소스를 누적, `extra` 에 각
소스 원본을 보존. (검색 품질·citation 비교에 유리.)

---

## 8. 융합 + 재랭킹

소스마다 정렬 기준이 달라 **점수를 그대로 합치면 깨진다**(arXiv·Semantic Scholar·OpenAlex 의 점수는 스케일·의미가
비교 불가). 업계 표준인 **순위 기반 융합 → (선택) 정밀 재랭크**의 2단으로 간다.

### Stage 1 — RRF 융합 (기본, 순수 Rust) ★

**Reciprocal Rank Fusion** 으로 소스별 랭킹을 *순위만으로* 융합한다. 점수 스케일에 무관·결정적·의존성 0.

```text
score(d) = Σ_sources  1 / (k + rank_source(d))      # k ≈ 60 (관례)
```

- Elasticsearch·OpenSearch·Azure AI Search 가 하이브리드 검색에 채택한 **사실상 표준**.
- **citation count·최신성(year)** 은 2차 가중치/타이브레이커로 결합한다.
- 같은 입력 결과 집합 → 항상 같은 최종 순위(결정적). (선택) `bm25` 로 질의-제목/초록 어휘 적합도를 보조 신호로 더할 수 있다.

### Stage 2 — cross-encoder 정밀 재랭크 (opt-in `rerank-onnx`) ★

Stage 1 의 top-N 만 신경망으로 재랭크해 정밀도를 끌어올린다.

- **cross-encoder(`bge-reranker` / `mxbai` / `jina`)** 를 ONNX 로 추론(`ort` FFI, 명시 opt-in).
  과학 도메인 재랭킹 벤치마크 **SciRerankBench(2025)** 에서 **cross-encoder 가 1위**(특히 "의미적으로 유사하나 논리적으로
  무관한" 까다로운 질의). SPLADE 는 최속이지만 추론이 약하다.
- 대안: 과학 특화 임베딩 **SPECTER2 / SciNCL** 로 dense 유사도를 결합(`embed-specter`). 특히 **Semantic Scholar API 가
  SPECTER 임베딩을 직접 제공**하므로, 어댑터에서 받아오면 로컬 ONNX 추론 없이도 의미 재랭크가 가능하다(영리한 우회).

> 🚫 **LLM 기반 listwise 재랭커(RankGPT 등)는 채택하지 않는다.** 동일 벤치마크에서 비용이 폭발적(수천 평가시간)인데
> 과학 도메인 정확도는 오히려 약했다(~33%). 비용·지연 대비 이득이 없다.

### 쿼리 확장 (HyDE) — caller 제공 경계

모호한 질의의 recall 은 **HyDE**(가상 답변 문서를 LLM 으로 생성→임베딩→검색)로 개선되지만, LLM·임베딩이 필요해
순수 Rust 경계를 벗어난다. → 오케스트레이터는 **확장된 쿼리를 받아 처리**만 한다(`SearchQuery.expansions: Vec<String>`).
HyDE 생성은 LLM 을 가진 Python 호출자 몫. "검색 코어는 결정적·경량, LLM 은 호출자"라는 경계를 지킨다.

---

## 9. 캐싱

두 레벨. 학술 메타데이터는 변경이 드물어 TTL 을 길게 잡아도 된다.

| 레벨 | 구현 | 성격 |
|---|---|---|
| **L1 in-process** | `moka` (Caffeine 포트, TTL + LRU) | 동일 쿼리 반복 시 API 호출 없이 즉시 반환 |
| **L2 disk (optional)** | `sled` 또는 `rusqlite` | 프로세스 재시작 후에도 유지. feature flag |

**캐시 키** = `(normalized_query, sources_fingerprint, page)`. 소스 조합이 달라지면 자동으로 캐시 미스가 발생한다.

---

## 10. Rate limit 관리

세 소스를 동시에 fan-out 하면 각 rate limit 에 동시에 부딪힌다. **소스별 독립 token bucket**(`governor` crate)을 둔다.

| 소스 | 권고 한도 | 정책 |
|---|---|---|
| arXiv | ~3초당 1요청 | governor token bucket + (필요 시) `tokio::time::sleep` |
| Semantic Scholar | 무인증 제한적(분당 한도) | API key 등록 시 상향 — 헤더에 key |
| OpenAlex | 사실상 무제한(공손 pool) | User-Agent 에 `mailto` 포함 시 빠른 pool 배정 |

---

## 11. 소스 어댑터 명세

| 소스 | 엔드포인트 | 응답 | 파싱 | rate limit | 구현 난이도 |
|---|---|---|---|---|---|
| **arXiv** | `export.arxiv.org/api/query` | Atom/XML | `quick-xml` (SAX, 메모리 효율) | ~1req/3s 권고 | 낮음 |
| **Semantic Scholar** | `api.semanticscholar.org/graph/v1/paper/search` | JSON | `serde_json` | 무인증 제한 / key 권장 | 낮음 |
| **OpenAlex** | `api.openalex.org/works` | JSON, cursor pagination | `serde_json` | 무료, mailto pool | 낮음~중간(스키마 복잡) |
| **Web (pluggable)** | Brave / Serper / Tavily / SearXNG | provider별 | provider별 | provider별 | trait 설계에 달림(인터페이스만 잘 잡으면 낮음) |

웹 어댑터는 유료/외부 의존이라 **별도 feature flag 또는 외부 crate**로 분리하고, 코어는 trait 만 정의한다.

---

## 12. PyO3 바인딩

> 목표 — "한 줄 import". Hugging Face `tokenizers`, `polars` 가 검증한 패턴.

### 동기 우선(sync-first) 설계 결정

PyO3 async 바인딩(`pyo3-async-runtimes`, 구 `pyo3-asyncio`)은 Python event loop ↔ tokio runtime 브리지를
다루는데, `asyncio.run()` 환경과 Jupyter(이미 loop 실행 중) 환경에서 동작이 미묘하게 다르다.

→ **v1 은 동기 메서드 우선**: 내부 tokio 런타임에서 `block_on` 으로 완료시켜 일반 함수처럼 노출(어디서나 안전).
async awaitable 변형은 검증된 테스트 케이스 확보 후 opt-in 으로 추가한다.

```rust
#[pymodule]
fn rust_scholar_transformer(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyOrchestrator>()?;
    Ok(())
}

#[pyclass]
struct PyOrchestrator { inner: std::sync::Arc<Orchestrator>, rt: tokio::runtime::Runtime }

#[pymethods]
impl PyOrchestrator {
    #[new]
    fn new(config: &Bound<'_, PyDict>) -> PyResult<Self> { /* ... */ }

    /// 동기 — 내부 런타임에서 완료. 결과는 JSON 직렬화하여 반환.
    fn search(&self, query: &str, limit: Option<usize>) -> PyResult<String> {
        let fut = self.inner.search(query, limit.unwrap_or(20));
        let papers = self.rt.block_on(fut).map_err(to_pyerr)?;
        serde_json::to_string(&papers).map_err(to_pyerr)
    }
}
```

```python
# Python 사용
import json
from rust_scholar_transformer import Orchestrator

orch = Orchestrator({"sources": ["arxiv", "semantic_scholar", "openalex"]})
papers = json.loads(orch.search("retrieval augmented generation 2024", limit=20))
```

빌드: `pyproject.toml` 에 maturin 백엔드, abi3 휠로 Python 3.9+ 호환.

```toml
[build-system]
requires = ["maturin>=1.5,<2.0"]
build-backend = "maturin"
```

---

## 13. Cargo Feature

```toml
[features]
# 기본: 학술 3소스 (무료·공개 API), 순수 Rust / zero-FFI
default            = ["arxiv", "semantic-scholar", "openalex"]

arxiv              = ["dep:quick-xml"]
semantic-scholar   = []          # serde_json (공통 의존)
openalex           = []          # serde_json (공통 의존)

# 웹 검색 provider 어댑터 — 유료/외부 의존이라 opt-in
web-brave          = []
web-tavily         = []
web-searxng        = ["dep:scraper"]

cache-disk         = ["dep:sled"]      # L2 디스크 캐시
rerank-onnx        = ["dep:ort"]       # Stage 2 cross-encoder 재랭킹 (FFI, 명시 opt-in)
embed-specter      = ["dep:ort"]       # SPECTER2/SciNCL dense 임베딩 재랭크 (FFI, 명시 opt-in)
python             = ["dep:pyo3"]      # PyO3 cdylib 바인딩
```

> default 는 학술 검색 + **RRF 융합 + MinHash-LSH dedup** 까지 순수 Rust 로 동작한다(FFI·유료 의존 0).
> 웹 provider·ONNX 재랭크/임베딩·디스크 캐시는 모두 명시 opt-in.

---

## 14. 빌딩블록(의존성)

| 컴포넌트 | 역할 | 성숙도 |
|---|---|---|
| `tokio` | 비동기 런타임, fan-out 기반(`JoinSet`) | 매우 높음 |
| `reqwest` | async HTTP (TLS·timeout·retry), `Client` 를 `Arc` 공유해 connection pool 재사용 | 매우 높음 |
| `serde` / `serde_json` | JSON 역직렬화 (Semantic Scholar·OpenAlex) | 매우 높음 |
| `quick-xml` | arXiv Atom feed 파싱 (SAX, 메모리 효율) | 높음 |
| `scraper` | HTML 파싱 (웹 어댑터 폴백) | 중간~높음 |
| `governor` | 소스별 token-bucket rate limit | 높음 |
| `moka` | L1 in-memory 캐시 (TTL+LRU) | 높음 |
| `ahash` | 제목 exact 해시(dedup) | 높음 |
| MinHash-LSH (`probminhash`/자체 구현) | 근접중복 dedup (전수 비교 회피) | 중간 |
| RRF (자체 구현, ~수십 줄) | 순위 융합 — crate 불필요, 결정적 | — |
| `bm25` | (선택) Stage 1 어휘 적합도 보조 점수 | 중간 |
| `async-trait` | `SourceAdapter` async trait | 매우 높음 |
| `pyo3` + `maturin` | Python 바인딩 / 휠 빌드 | 높음 |
| `sled` / `rusqlite` | L2 디스크 캐시 (opt-in) | 높음 |
| `ort` (opt-in) | Stage 2 cross-encoder / SPECTER2 ONNX 추론 (FFI) | 높음 |

전부 순수 Rust (ONNX `ort` 만 FFI, opt-in). 기존 `rust_markdown_transformer` 정체성과 동일.

---

## 15. 주의해야 할 실제 난점

1. **스키마 정규화(최대 작업량)** — 소스별 저자·식별자·메타 표현 차이를 공통 `Paper` 로 흡수하는 레이어가 핵심.
   `extra: HashMap` 로 원본 보존 + 핵심 필드 강타입. 초기에 잘못 잡으면 전체를 다시 뜯게 된다.
2. **Rate limit 동시 충돌** — fan-out 시 각 소스 한도에 동시에 부딪힘. `governor` 를 **소스별 독립 인스턴스**로.
3. **재랭킹 범위** — BM25 + citation 가중치로 시작. cross-encoder 신경망은 ONNX(opt-in) 또는 Python 으로 분리.
4. **PyO3 async 복잡성** — event loop ↔ tokio 브리지가 환경별로 미묘. **동기 우선**으로 회피하고 async 는 후순위.

---

## 16. 개발 로드맵

| 단계 | 내용 | 난이도 |
|---|---|---|
| **Phase 1** | `SourceAdapter` trait + arXiv 어댑터 + `Paper`/`serde` 스키마 + 정규화 골격 | 낮음 |
| **Phase 2** | Semantic Scholar + OpenAlex 어댑터 + 정규화 레이어 완성 | 중간 |
| **Phase 3** | Fan-out 엔진 + dedup(DOI/arXiv/title) + **RRF 융합** + citation/recency + L1 캐시 + rate limit | 중간 |
| **Phase 4** | PyO3 바인딩(sync-first) + maturin 빌드 파이프라인 | 중간 |
| **Phase 5** | **MinHash-LSH 근접중복** + L2 디스크 캐시(opt-in) | 중간 |
| **Phase 6** | Pluggable 웹 검색 어댑터 인터페이스(+ 1개 레퍼런스 provider) | 낮음(인터페이스 중심) |
| **Phase 7** | (opt-in) Stage 2 cross-encoder ONNX 재랭크 + SPECTER2 dense 재랭크 | 중간(FFI) |

---

## 17. 디렉토리 구조 (제안)

```text
rust_scholar_transformer/
  Cargo.toml
  README.md              # 이 문서(설계)
  LICENSE                # Apache-2.0
  pyproject.toml         # maturin (feature = "python")
  src/
    lib.rs               # 크레이트 루트 · re-export
    model.rs             # Paper / Author / PaperIdentity / SearchQuery / RawResult
    error.rs             # AdapterError / OrchestratorError
    orchestrator.rs      # Orchestrator (fan-out · 정규화 · dedup · rerank · cache 조립)
    adapter.rs           # SourceAdapter trait + 레지스트리
    fanout.rs            # JoinSet 기반 동시 요청
    dedup.rs             # PaperIdentity(DOI/arXiv/title) + MinHash-LSH 근접중복 병합
    fusion.rs            # RRF 순위 융합 + citation/recency
    rerank.rs            # (opt-in) cross-encoder / SPECTER2 ONNX 재랭크
    cache.rs             # L1(moka) + L2(sled, opt-in)
    ratelimit.rs         # governor 소스별
    normalize.rs         # 소스별 RawResult → Paper
    adapters/
      arxiv.rs  semantic_scholar.rs  openalex.rs
      web/                # pluggable 웹 provider (opt-in)
    python.rs            # PyO3 바인딩 (feature = "python")
  tests/
    integration.rs       # 모킹 HTTP(wiremock) 기반 결정적 테스트
  benches/
```

> 네트워크 의존 테스트는 `wiremock` 등으로 **HTTP 응답을 모킹**해 결정적으로 검증한다(라이브 API 의존 회피).

---

## 18. 라이선스

Apache-2.0

---

## 총평 (설계 판단)

기술적 실현 가능성이 높고, 선택 스택(reqwest + tokio + serde + quick-xml/scraper + PyO3)이 용도에 정확히 맞으며,
학술 3소스가 모두 무료·공개 API 라 상업적 제약 없이 배포 가능하다. "Rust = retrieval engine" 정체성과도 일치한다.

가장 큰 리스크는 기술이 아니라 **공통 `Paper` 정규화 스키마를 얼마나 초기에 유연하게 잡느냐**다.
핵심 필드는 강타입, 소스별 원본은 `extra` 로 보존하는 설계를 Phase 1 에서 확정하면 이후가 순탄하다.

---

## 참고 문헌·기법

검색 품질을 좌우하는 핵심 알고리즘의 근거. 구현 우선순위는 본문 §7·§8 참조.

- **Reciprocal Rank Fusion (RRF)** — 이종 소스 랭킹 융합의 사실상 표준. Elasticsearch / OpenSearch / Azure AI Search 채택.
  - https://www.elastic.co/guide/en/elasticsearch/reference/current/rrf.html
  - https://opensearch.org/blog/introducing-reciprocal-rank-fusion-hybrid-search/
- **SciRerankBench: Benchmarking Rerankers Towards Scientific RAG (2025)** — 과학 도메인에서 cross-encoder 우위, LLM 재랭커 비효율 결론 - https://arxiv.org/abs/2508.08742
- **CoRank: LLM-Based Compact Reranking with Document Features for Scientific Retrieval (2025)** - https://arxiv.org/abs/2505.13757
- **SPECTER2 / SciNCL** — 과학 논문 임베딩(인용 기반) - https://huggingface.co/allenai/specter2
- **BGE Reranker** — cross-encoder 재랭커 - https://huggingface.co/BAAI/bge-reranker-large
- **LSHBloom: Memory-efficient, Extreme-scale Document Deduplication (2024)** — MinHash-LSH 근접중복 - https://arxiv.org/abs/2411.04257
- **HyDE: Precise Zero-Shot Dense Retrieval without Relevance Labels** — 가상 문서 임베딩 쿼리 확장 - https://arxiv.org/abs/2212.10496
