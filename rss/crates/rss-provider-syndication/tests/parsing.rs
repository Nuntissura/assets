use rss_provider_syndication::{blogs, rss};
use rss_contracts::{Bounds, Error};

fn bounds() -> Bounds { Bounds { max_url_bytes: 4096, max_text_bytes: 4096, max_collection_items: 100, max_page_items: 2, max_operation_bytes: 100_000 } }
fn feed(items: &str) -> String { format!(r#"<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/"><channel><title>Blog</title><link>https://example.org/</link><description>Blog feed</description>{items}</channel></rss>"#) }

#[test]
fn rss_preserves_explicit_identity_publication_and_complementary_ordered_media() {
    let xml = feed(r#"<item><guid isPermaLink="false">opaque-7</guid><link>https://example.org/post#section</link><title>First &amp; second</title><description><![CDATA[<p>Readable <b>text</b></p>]]></description><author>Writer</author><pubDate>Fri, 23 Oct 2026 12:34:56 +0200</pubDate><category>Rust</category><enclosure url="https://media.example/movie.mp4" type="video/mp4" length="1234"/><media:content url="https://media.example/movie.mp4" type="video/mp4" width="640" height="360" fileSize="1234" duration="2"/><media:thumbnail url="https://media.example/poster.jpg" width="120" height="80"/></item>"#);
    let doc = rss::parse("https://example.org/feed", xml.as_bytes(), 100_000, 100, &bounds()).unwrap();
    let item = &doc.entries[0];
    assert_eq!(item.key, "opaque-7"); assert_eq!(item.reliable_provider_id.as_deref(), Some("opaque-7"));
    assert_eq!(item.url, "https://example.org/post#section"); assert_eq!(item.title.as_deref(), Some("First & second"));
    assert_eq!(item.description.as_deref(), Some("Readable text")); assert_eq!(item.labels, ["Rust"]);
    assert_eq!(item.author.as_deref(), Some("Writer"));
    let publication = item.published_at.as_ref().unwrap();
    publication.validate().unwrap();
    assert_eq!(publication.raw_value, "Fri, 23 Oct 2026 12:34:56 +0200"); assert_eq!(publication.offset_minutes, Some(120));
    assert_eq!(item.media.len(), 2); assert_eq!(item.media[0].width, Some(640)); assert_eq!(item.media[0].duration_ms, Some(2000));
    assert_eq!(item.media[0].size_bytes, Some(1234)); assert!(!item.media[0].thumbnail); assert!(item.media[1].thumbnail);
}

#[test]
fn atom_resolves_relative_links_and_explicit_next_document() {
    let xml = br#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://example.org/blog/"><id>urn:blog</id><title>Blog</title><updated>2026-10-22T00:00:00Z</updated><link rel="next" href="archive.atom"/><entry><id>urn:post:7</id><title>Post</title><author><email>writer@example.org</email></author><updated>2026-10-22T01:00:00Z</updated><published>2026-10-23T12:34:56.000+02:00</published><link rel="alternate" type="text/html" href="post"/><link rel="enclosure" type="audio/mpeg" href="sound.mp3"/></entry></feed>"#;
    let doc = rss::parse("https://example.org/blog/feed.atom", xml, 100_000, 100, &bounds()).unwrap();
    assert_eq!(doc.next_document.as_deref(), Some("https://example.org/blog/archive.atom"));
    assert_eq!(doc.entries[0].url, "https://example.org/blog/post"); assert_eq!(doc.entries[0].key, "urn:post:7");
    assert!(doc.entries[0].author.is_none());
    assert_eq!(doc.entries[0].media[0].url, "https://example.org/blog/sound.mp3");
    assert_eq!(doc.entries[0].published_at.as_ref().unwrap().raw_value, "2026-10-23T12:34:56.000+02:00");
    let publication = doc.entries[0].published_at.as_ref().unwrap(); publication.validate().unwrap();
    assert_eq!(publication.precision, hsscrap::PublicationPrecision::Subsecond);
    assert_eq!(publication.local_value, "2026-10-23T12:34:56.000");
}

#[test]
fn missing_guid_uses_url_identity_across_unchanged_refresh_and_title_edits() {
    let first = feed("<item><link>https://example.org/post</link><title>Before</title></item>");
    let changed = first.replace("Before", "After");
    let a = rss::parse("https://example.org/feed", first.as_bytes(), 100_000, 100, &bounds()).unwrap();
    let again = rss::parse("https://example.org/feed", first.as_bytes(), 100_000, 100, &bounds()).unwrap();
    let b = rss::parse("https://example.org/feed", changed.as_bytes(), 100_000, 100, &bounds()).unwrap();
    assert_eq!(a.entries, again.entries); assert_eq!(a.entries[0].key, b.entries[0].key);
    assert!(a.entries[0].reliable_provider_id.is_none()); assert_ne!(a.digest, b.digest);
}

#[test]
fn encoded_bytes_are_parsed_without_lossy_http_text_decoding() {
    let mut xml = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><rss version=\"2.0\"><channel><title>Blog</title><link>https://example.org/</link><description>Blog</description><item><guid>x</guid><link>https://example.org/x</link><title>Caf".to_vec();
    xml.push(0xe9); xml.extend_from_slice(b"</title></item></channel></rss>");
    let doc = rss::parse("https://example.org/feed", &xml, 100_000, 100, &bounds()).unwrap();
    assert_eq!(doc.entries[0].title.as_deref(), Some("Café"));
}

#[test]
fn malformed_oversized_and_untraceable_entries_fail_without_silent_cutoff() {
    assert!(rss::parse("https://example.org/feed", b"<invalid", 100_000, 100, &bounds()).is_err());
    let xml = feed("<item><guid>x</guid><link>https://example.org/x</link></item>");
    assert!(matches!(rss::parse("https://example.org/feed", xml.as_bytes(), 5, 100, &bounds()), Err(Error::LimitExceeded { .. })));
    let no_link = feed("<item><guid>urn:no-url</guid><title>No usable link</title></item>");
    assert!(rss::parse("https://example.org/feed", no_link.as_bytes(), 100_000, 100, &bounds()).is_err());
    let many = feed("<item><guid>x</guid><link>https://example.org/x</link></item><item><guid>y</guid><link>https://example.org/y</link></item>");
    assert!(matches!(rss::parse("https://example.org/feed", many.as_bytes(), 100_000, 1, &bounds()), Err(Error::LimitExceeded { .. })));
}

#[test]
fn duplicate_provider_ids_cannot_retarget_an_entry() {
    let xml = feed("<item><guid>same</guid><link>https://example.org/x</link></item><item><guid>same</guid><link>https://example.org/y</link></item>");
    assert!(matches!(rss::parse("https://example.org/feed", xml.as_bytes(), 100_000, 100, &bounds()), Err(Error::Conflict)));
}

#[test]
fn discovery_returns_explicit_candidates_and_rejects_non_web_targets() {
    let html = br#"<!doctype html><link rel="alternate stylesheet" type="application/rss+xml" href="/feed.xml"><link rel="ALTERNATE" type="application/atom+xml; charset=utf-8" href="posts.atom">"#;
    let feeds = blogs::discover("https://example.org/blog/", html, 100_000, &bounds()).unwrap();
    assert_eq!(feeds, ["https://example.org/blog/posts.atom", "https://example.org/feed.xml"]);
    assert!(blogs::public_url("file:///private", &bounds()).is_err());
    assert!(blogs::public_url("https://user:secret@example.org/feed", &bounds()).is_err());
}

#[test]
fn uncertain_publication_is_not_replaced_with_fetch_time() {
    let xml = feed("<item><guid>x</guid><link>https://example.org/x</link><pubDate>2026</pubDate></item>");
    let doc = rss::parse("https://example.org/feed", xml.as_bytes(), 100_000, 100, &bounds()).unwrap();
    assert!(doc.entries[0].published_at.is_none());
}

#[test]
fn minus_zero_offsets_remain_unknown_while_known_utc_stays_known() {
    for (literal, expected) in [("2026-10-23T12:34:56-00:00", None), ("2026-10-23T12:34:56Z", Some(0)),
        ("2026-10-23T12:34:56+00:00", Some(0)), ("Fri, 23 Oct 2026 12:34:56 -0000", None),
        ("Fri, 23 Oct 2026 12:34:56 +0000", Some(0))] {
        let xml = feed(&format!("<item><guid>x</guid><link>https://example.org/x</link><pubDate>{literal}</pubDate></item>"));
        let doc = rss::parse("https://example.org/feed", xml.as_bytes(), 100_000, 100, &bounds()).unwrap();
        let publication = doc.entries[0].published_at.as_ref().unwrap(); publication.validate().unwrap();
        assert_eq!(publication.raw_value, literal); assert_eq!(publication.offset_minutes, expected);
        assert_eq!(publication.local_value, "2026-10-23T12:34:56");
    }
}

#[test]
fn leap_seconds_preserve_the_source_second_in_canonical_publication() {
    for literal in ["2016-12-31T23:59:60Z", "2016-12-31T23:59:60.000Z", "Sat, 31 Dec 2016 23:59:60 +0000"] {
        let xml = feed(&format!("<item><guid>x</guid><link>https://example.org/x</link><pubDate>{literal}</pubDate></item>"));
        let doc = rss::parse("https://example.org/feed", xml.as_bytes(), 100_000, 100, &bounds()).unwrap();
        let publication = doc.entries[0].published_at.as_ref().unwrap(); publication.validate().unwrap();
        assert_eq!(publication.raw_value, literal); assert_eq!(publication.local_value.get(17..19), Some("60"));
        assert_eq!(publication.offset_minutes, Some(0));
    }
}
