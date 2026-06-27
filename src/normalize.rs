//! 정규화 헬퍼 — URL 정규화, 제목 정리, 정체성 해시.

/// 추적 파라미터를 제거하고 trailing slash 를 정리한 정규 URL 을 만든다.
pub fn canonicalize_url(raw: &str) -> Option<String> {
    const TRACKING: [&str; 7] = [
        "utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content", "fbclid", "gclid",
    ];
    let mut parsed = url::Url::parse(raw).ok()?;
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !TRACKING.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut qp = parsed.query_pairs_mut();
        qp.clear();
        for (k, v) in &kept {
            qp.append_pair(k, v);
        }
    }
    // 쿼리가 비면 `?` 자체를 제거.
    if parsed.query() == Some("") {
        parsed.set_query(None);
    }
    let s = parsed.to_string();
    Some(s.trim_end_matches('/').to_string())
}

/// 개행·중복 공백 정리.
pub fn normalize_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 제목을 소문자화 + 영숫자만 남겨 결정적 해시(FNV-1a 64bit)로 만든다.
/// 정체성 식별자(DOI/arXiv ID/canonical URL)가 없을 때의 중복제거 폴백 키.
pub fn title_hash(title: &str) -> u64 {
    let cleaned: String = title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    fnv1a_64(cleaned.as_bytes())
}

/// 날짜 문자열을 UTC 시각으로 파싱한다. RFC3339(`2024-01-01T00:00:00Z`) 우선, 실패 시
/// 날짜만(`2024-01-01`)을 자정 UTC 로 해석한다.
pub fn parse_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDate, Utc};
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d
            .and_hms_opt(0, 0, 0)
            .map(|ndt| DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc));
    }
    None
}

/// HTML 태그를 제거하고 흔한 엔티티를 디코드한 뒤 공백을 정리한다(피드 요약·본문 정제용).
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// FNV-1a 64bit — 의존성 없는 결정적 해시(런 간 안정).
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tracking_params() {
        let got = canonicalize_url("https://example.com/post?id=7&utm_source=x&gclid=abc").unwrap();
        assert_eq!(got, "https://example.com/post?id=7");
    }

    #[test]
    fn drops_trailing_slash_and_empty_query() {
        let got = canonicalize_url("https://example.com/post/?utm_medium=y").unwrap();
        assert_eq!(got, "https://example.com/post");
    }

    #[test]
    fn title_hash_is_deterministic_and_normalizing() {
        // 대소문자·구두점·공백 차이는 같은 해시로 수렴.
        assert_eq!(title_hash("Multi-Agent RAG!"), title_hash("multi agent rag"));
        // 다른 제목은 다른 해시.
        assert_ne!(title_hash("attention is all you need"), title_hash("deep residual learning"));
    }

    #[test]
    fn normalize_title_collapses_whitespace() {
        assert_eq!(normalize_title("  a\n  b   c "), "a b c");
    }

    #[test]
    fn strip_html_removes_tags_and_decodes_entities() {
        assert_eq!(strip_html("<p>Hello &amp; <b>world</b></p>"), "Hello & world");
    }
}
