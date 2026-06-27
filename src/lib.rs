//! # rust_scholar_transformer
//!
//! 실시간 멀티소스 리트리벌 코어 엔진. 여러 출처(arXiv·뉴스·블로그·유튜브·웹)를 동시에
//! 가져와 단일 [`Document`] 타입으로 정규화하고, 중복제거·순위융합·캐싱을 거쳐 상위 결과를
//! 돌려준다. LLM·Python 이 만능 검색·만능 논문 조회에 쓰는 순수 Rust 도구다.
//!
//! 핵심 경계: 결정적·기계적·I/O·CPU 작업(수집·파싱·정규화·중복제거·순위융합·캐싱)은 코어가,
//! 의미 이해·생성·판단(질의 의도·쿼리 확장·요약·답변)은 호출자(LLM/Python)가 맡는다.
//! 코어는 기본 빌드에 신경망·LLM 의존성이 없다.
//!
//! ## 현재 구현
//! - [`Document`]/[`SearchQuery`] 스키마, [`Source`] 트레이트
//! - arXiv 어댑터([`sources::ArxivSource`]) — 라이브 검색 API 경로, HTTP status 선확인 + 429 백오프
//! - 블로그/RSS 구독 어댑터([`sources::RssSource`]) · Google News 검색 어댑터([`sources::GoogleNewsSource`])
//! - 정체성 기반 중복제거([`dedup`]) + RRF 순위 융합 + 신선도·신뢰도 2차 신호([`fusion`])
//! - fan-out [`Engine`] — 동시 실행 + 부분 실패를 [`SearchReport`] 로 표면화
//!
//! 후속 Phase(arXiv OAI-PMH 1순위 경로 + 로컬 인덱스, 유튜브·웹 provider, MinHash-LSH
//! 근접중복, 캐시, PyO3 바인딩)는 README 참조.

pub mod cache;
pub mod dedup;
pub mod engine;
pub mod error;
pub mod fusion;
pub mod model;
pub mod normalize;
pub mod ratelimit;
pub mod source;
pub mod sources;

#[cfg(feature = "python")]
mod python;

pub use cache::{Cache, MemoryCache};
pub use engine::Engine;
pub use error::{ConnectorWarning, FetchError, SearchReport};
pub use model::{Author, DocIdentity, Document, Recency, Score, SearchQuery, SourceKind};
pub use ratelimit::MinIntervalLimiter;
pub use source::{RatePolicy, Source};
pub use sources::{
    ArxivOaiSource, ArxivSource, BraveProvider, FeedSource, GoogleNewsSource, RssSource, WebProvider,
    WebSource, YoutubeSource,
};
