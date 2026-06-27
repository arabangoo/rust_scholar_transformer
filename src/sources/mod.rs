//! 소스 어댑터들. 각 어댑터는 [`crate::source::Source`] 를 구현한다.
//!
//! - arXiv: [`ArxivOaiSource`](arxiv_oai::ArxivOaiSource) (OAI-PMH, 1순위) · [`ArxivSource`](arxiv::ArxivSource) (라이브 API, 보조)
//! - 블로그/RSS 구독: [`RssSource`](rss::RssSource)
//! - Google News 검색: [`GoogleNewsSource`](news::GoogleNewsSource)
//! - 유튜브 메타데이터: [`YoutubeSource`](youtube::YoutubeSource)
//! - 웹 검색(provider 추상화): [`WebSource`](web::WebSource) + [`BraveProvider`](web::BraveProvider)

pub mod arxiv;
pub mod arxiv_oai;
pub(crate) mod feed_common;
pub mod news;
pub mod rss;
pub mod web;
pub mod youtube;

pub use arxiv::ArxivSource;
pub use arxiv_oai::ArxivOaiSource;
pub use news::GoogleNewsSource;
pub use rss::{FeedSource, RssSource};
pub use web::{BraveProvider, WebProvider, WebSource};
pub use youtube::YoutubeSource;
