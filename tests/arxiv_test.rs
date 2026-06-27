//! arXiv 어댑터 + 엔진 통합 테스트. 라이브 API 대신 wiremock 으로 HTTP 응답을 모킹해
//! 결정적으로 검증한다(라이브 호출·rate limit 의존 회피).

use std::time::Duration;

use rust_scholar_transformer::{
    ArxivSource, Engine, FetchError, Recency, SearchQuery, Source, SourceKind,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2301.00001v2</id>
    <title>Sample Title One</title>
    <published>2023-01-01T00:00:00Z</published>
    <summary>First summary.</summary>
    <author><name>Alice A</name></author>
    <author><name>Bob B</name></author>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2302.12345v1</id>
    <title>Sample Title Two</title>
    <published>2023-02-02T00:00:00Z</published>
    <summary>Second summary.</summary>
    <author><name>Carol C</name></author>
  </entry>
</feed>"#;

#[tokio::test]
async fn arxiv_search_parses_and_normalizes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM))
        .mount(&server)
        .await;

    let src = ArxivSource::new().with_base_url(format!("{}/api/query", server.uri()));
    let docs = src.search(&SearchQuery::from_text("multi agent rag", 10)).await.unwrap();

    assert_eq!(docs.len(), 2);
    let d0 = &docs[0];
    assert_eq!(d0.source, SourceKind::Arxiv);
    assert_eq!(d0.title, "Sample Title One");
    assert_eq!(d0.identity.arxiv_id.as_deref(), Some("2301.00001"));
    assert_eq!(d0.authors.len(), 2);
    assert!(d0.published_at.is_some());
    assert_eq!(d0.language.as_deref(), Some("en"));
}

#[tokio::test]
async fn arxiv_gives_up_after_persistent_429() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let src = ArxivSource::new()
        .with_base_url(format!("{}/api/query", server.uri()))
        .with_retry(2, Duration::ZERO);
    let err = src.search(&SearchQuery::from_text("x", 5)).await.unwrap_err();
    match err {
        FetchError::Http(m) => assert!(m.contains("429"), "expected 429 in: {m}"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn engine_fanout_collects_and_fuses_with_rrf() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM))
        .mount(&server)
        .await;

    let mut engine = Engine::new();
    engine.register(Box::new(
        ArxivSource::new().with_base_url(format!("{}/api/query", server.uri())),
    ));

    let report = engine.search(SearchQuery::from_text("agentic", 10)).await;
    assert_eq!(report.docs.len(), 2);
    assert!(report.warnings.is_empty());
    // 단일 소스에서 RRF 는 소스가 돌려준 순서를 보존 → ATOM 첫 항목(2301...)이 1위.
    assert_eq!(report.docs[0].identity.arxiv_id.as_deref(), Some("2301.00001"));
    assert!(report.docs[0].score.fused > report.docs[1].score.fused);
}

#[tokio::test]
async fn engine_recency_filter_drops_old_docs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM))
        .mount(&server)
        .await;

    let mut engine = Engine::new();
    engine.register(Box::new(
        ArxivSource::new().with_base_url(format!("{}/api/query", server.uri())),
    ));

    // ATOM 의 문서는 2023년 게시 → recency=Day(최근 1일)면 전부 필터링된다.
    let mut q = SearchQuery::from_text("agentic", 10);
    q.recency = Some(Recency::Day);
    let report = engine.search(q).await;

    assert_eq!(report.docs.len(), 0); // 오래된 문서는 날짜 필터에 걸려 제거
    assert!(report.warnings.is_empty()); // 소스는 성공, 단지 필터링됨

    // recency 가 없으면 그대로 2건(대조).
    let report2 = engine.search(SearchQuery::from_text("agentic", 10)).await;
    assert_eq!(report2.docs.len(), 2);
}
