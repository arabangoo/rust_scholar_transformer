//! 블로그/RSS·Google News 어댑터 + 멀티소스 엔진(fan-out + RRF 융합) 통합 테스트.
//! 라이브 호출 대신 wiremock 으로 결정적 검증.

use rust_scholar_transformer::{
    ArxivSource, Engine, FeedSource, GoogleNewsSource, RssSource, SearchQuery, Source, SourceKind,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <item>
      <title>Agentic AI breakthrough</title>
      <link>https://blog.example.com/agentic?utm_source=rss</link>
      <pubDate>Wed, 01 Jan 2025 00:00:00 GMT</pubDate>
      <description>A post about agentic systems and loops.</description>
    </item>
    <item>
      <title>Unrelated cooking post</title>
      <link>https://blog.example.com/cooking</link>
      <pubDate>Tue, 31 Dec 2024 00:00:00 GMT</pubDate>
      <description>Recipes and food.</description>
    </item>
  </channel>
</rss>"#;

const ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2301.00001v2</id>
    <title>Arxiv Paper One</title>
    <published>2023-01-01T00:00:00Z</published>
    <summary>First.</summary>
    <author><name>Alice</name></author>
  </entry>
</feed>"#;

#[tokio::test]
async fn rss_blog_parses_filters_and_canonicalizes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS))
        .mount(&server)
        .await;

    let src = RssSource::new(vec![FeedSource::new("Test", format!("{}/feed", server.uri()))]);
    let docs = src.search(&SearchQuery::from_text("agentic", 10)).await.unwrap();

    // "agentic" 키워드 필터 -> cooking 항목은 제외, 1건만.
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].source, SourceKind::Blog);
    assert_eq!(docs[0].title, "Agentic AI breakthrough");
    // utm 추적 파라미터 제거된 canonical URL.
    assert_eq!(docs[0].identity.canonical_url.as_deref(), Some("https://blog.example.com/agentic"));
    assert!(docs[0].published_at.is_some());
}

#[tokio::test]
async fn google_news_search_returns_all_items_unfiltered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rss/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS))
        .mount(&server)
        .await;

    let src = GoogleNewsSource::new().with_base_url(format!("{}/rss/search", server.uri()));
    let docs = src.search(&SearchQuery::from_text("anything", 10)).await.unwrap();

    // 검색형이라 클라이언트 필터 없음 -> 2건 모두.
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].source, SourceKind::News);
}

#[tokio::test]
async fn engine_fans_out_across_arxiv_and_blog() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS))
        .mount(&server)
        .await;

    let mut engine = Engine::new();
    engine.register(Box::new(
        ArxivSource::new().with_base_url(format!("{}/api/query", server.uri())),
    ));
    engine.register(Box::new(RssSource::new(vec![FeedSource::new(
        "Test",
        format!("{}/feed", server.uri()),
    )])));

    let report = engine.search(SearchQuery::from_text("agentic", 20)).await;

    // arXiv 1건 + 블로그(agentic 필터) 1건 = 2건, 서로 다른 정체성이라 병합 없음.
    assert_eq!(report.docs.len(), 2);
    assert!(report.warnings.is_empty());
    let kinds: Vec<SourceKind> = report.docs.iter().map(|d| d.source).collect();
    assert!(kinds.contains(&SourceKind::Arxiv));
    assert!(kinds.contains(&SourceKind::Blog));
    // 모든 결과에 RRF 융합 점수가 매겨져 있다.
    assert!(report.docs.iter().all(|d| d.score.fused > 0.0));

    // 피드별 신뢰도를 안 줬으면 authority 는 SourceKind 기본값(arXiv 0.90 / 블로그 0.85).
    let arxiv = report.docs.iter().find(|d| d.source == SourceKind::Arxiv).unwrap();
    let blog = report.docs.iter().find(|d| d.source == SourceKind::Blog).unwrap();
    assert!((arxiv.score.authority - 0.90).abs() < 1e-9);
    assert!((blog.score.authority - 0.85).abs() < 1e-9);
}

#[tokio::test]
async fn rss_per_feed_reliability_overrides_authority() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS))
        .mount(&server)
        .await;

    let mut engine = Engine::new();
    engine.register(Box::new(RssSource::new(vec![
        FeedSource::new("Trusted", format!("{}/feed", server.uri())).with_reliability(0.99),
    ])));

    let report = engine.search(SearchQuery::from_text("agentic", 10)).await;
    assert_eq!(report.docs.len(), 1);
    // 피드별 신뢰도 0.99 가 SourceKind 기본값(0.85) 대신 authority 로 쓰인다.
    assert!((report.docs[0].score.authority - 0.99).abs() < 1e-9);
}
