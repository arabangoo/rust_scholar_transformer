//! 공통 데이터 모델 — 모든 소스가 수렴하는 단일 문서 타입과 질의 타입.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 출처 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceKind {
    Arxiv,
    News,
    Blog,
    Youtube,
    Web,
}

/// 저자.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    pub id: Option<String>,
}

impl Author {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), id: None }
    }
}

/// 중복제거 기준이 되는 문서 정체성.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocIdentity {
    /// 학술 문서 최우선 식별자.
    pub doi: Option<String>,
    /// arXiv ID — 정규화 형태(`2301.00001v2` -> `2301.00001`).
    pub arxiv_id: Option<String>,
    /// 추적 파라미터를 제거한 정규 URL.
    pub canonical_url: Option<String>,
    /// 소문자화 + 특수문자 제거 후 해시. 정체성 식별자가 없을 때의 폴백.
    pub title_hash: u64,
}

/// 재랭킹 점수. 1차 정렬축은 `fused`(RRF), 나머지는 2차 신호.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub fused: f64,
    pub freshness: f64,
    pub authority: f64,
    pub relevance: f64,
}

/// 모든 소스가 수렴하는 단일 문서 타입.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub identity: DocIdentity,
    pub source: SourceKind,
    pub title: String,
    pub url: String,
    pub authors: Vec<Author>,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub language: Option<String>,
    pub tags: Vec<String>,
    /// 같은 문서를 제공한 소스들(병합 추적).
    pub sources: Vec<SourceKind>,
    pub score: Score,
    /// 소스별 원본 메타 보존.
    pub extra: HashMap<String, serde_json::Value>,
}

/// 신선도 윈도우.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Recency {
    Day,
    Week,
    Month,
    Year,
}

/// 출처 비의존 질의.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub text: String,
    /// 비어 있으면 등록된 모든 소스를 대상으로 한다.
    pub sources: Vec<SourceKind>,
    /// 통합 top-K(소스별 fetch 수가 아니라 최종 반환 개수).
    pub limit: usize,
    pub language: Option<String>,
    pub recency: Option<Recency>,
    pub from_date: Option<DateTime<Utc>>,
    pub to_date: Option<DateTime<Utc>>,
    /// 전문 추출 여부(비용·속도 영향). Phase 1 arXiv 에서는 미적용.
    pub include_content: bool,
    /// 호출자(LLM)가 제공하는 쿼리 확장. 코어는 받아 처리만 한다.
    pub expansions: Vec<String>,
}

impl SearchQuery {
    /// 텍스트 + 개수로 기본 질의를 만든다(나머지는 기본값).
    pub fn from_text(text: impl Into<String>, limit: usize) -> Self {
        Self {
            text: text.into(),
            sources: Vec::new(),
            limit,
            language: None,
            recency: None,
            from_date: None,
            to_date: None,
            include_content: false,
            expansions: Vec::new(),
        }
    }

    /// 대상 소스를 한정한다.
    pub fn with_sources(mut self, sources: Vec<SourceKind>) -> Self {
        self.sources = sources;
        self
    }
}
