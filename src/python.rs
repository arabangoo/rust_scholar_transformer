//! PyO3 바인딩 — `feature = "python"` 활성 시 cdylib 으로 빌드되어 `import rust_scholar_transformer`
//! 로 사용한다. abi3(Python 3.9+) 단일 휠. 동기 우선(sync-first): 내부 tokio 런타임에서 block_on
//! 으로 완료시켜 일반 함수처럼 노출한다(asyncio/Jupyter 환경 차이 회피). 결과는 JSON 문자열.
//!
//! ```python
//! from rust_scholar_transformer import Retriever
//! r = Retriever(sources=["arxiv", "news"])
//! docs = r.search("agentic loop engineering", limit=20)  # JSON 문자열
//! ```

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyModule;

use crate::sources::{
    ArxivOaiSource, BraveProvider, FeedSource, GoogleNewsSource, RssSource, WebSource, YoutubeSource,
};
use crate::{Engine, SearchQuery};

#[pyclass]
struct Retriever {
    engine: Engine,
    rt: tokio::runtime::Runtime,
}

#[pymethods]
impl Retriever {
    /// 소스 목록과 자격증명으로 리트리버를 만든다.
    /// sources: "arxiv" | "news" | "blog" | "youtube" | "web" (기본 ["arxiv","news"]).
    #[new]
    #[pyo3(signature = (sources=None, rss_feeds=None, youtube_api_key=None, brave_api_key=None))]
    fn new(
        sources: Option<Vec<String>>,
        rss_feeds: Option<Vec<String>>,
        youtube_api_key: Option<String>,
        brave_api_key: Option<String>,
    ) -> PyResult<Self> {
        let rt = tokio::runtime::Runtime::new().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let mut engine = Engine::new();
        let wanted = sources.unwrap_or_else(|| vec!["arxiv".to_string(), "news".to_string()]);

        for s in &wanted {
            match s.as_str() {
                "arxiv" => {
                    engine.register(Box::new(ArxivOaiSource::new()));
                }
                "news" => {
                    engine.register(Box::new(GoogleNewsSource::new()));
                }
                "blog" => {
                    if let Some(feeds) = &rss_feeds {
                        let fs = feeds.iter().map(|u| FeedSource::new("feed", u.clone())).collect();
                        engine.register(Box::new(RssSource::new(fs)));
                    }
                }
                "youtube" => {
                    if let Some(k) = &youtube_api_key {
                        engine.register(Box::new(YoutubeSource::new(k.clone())));
                    }
                }
                "web" => {
                    if let Some(k) = &brave_api_key {
                        engine.register(Box::new(WebSource::new(Box::new(BraveProvider::new(
                            k.clone(),
                        )))));
                    }
                }
                _ => {}
            }
        }
        Ok(Self { engine, rt })
    }

    /// 질의를 실행하고 결과를 JSON 문자열로 돌려준다(동기). 내부에서 동시 fan-out + 융합 + 중복제거.
    #[pyo3(signature = (query, limit=20))]
    fn search(&self, query: &str, limit: usize) -> PyResult<String> {
        let q = SearchQuery::from_text(query, limit);
        let report = self.rt.block_on(self.engine.search(q));
        serde_json::to_string(&report).map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }
}

#[pymodule]
fn rust_scholar_transformer(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Retriever>()?;
    Ok(())
}
