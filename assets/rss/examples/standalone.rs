//! A separate synthetic consumer. No host, database, network fetch or credential is used.
use rss_acquisition::jobs::{JobLimits, JobQueue, JobState};
use rss_contracts::*;
use rss_core::{query::*, ProfileSelection, SourceGroup, SourceLibrary};
use serde_json::json;
use std::collections::BTreeSet;
#[cfg(feature = "gui")]
mod browser;
fn reference(id: &str) -> HostRef {
    HostRef {
        host_instance_id: "standalone".into(),
        owner_kind: "example".into(),
        native_id: id.into(),
    }
}
fn bounds() -> Bounds {
    Bounds {
        max_url_bytes: 4096,
        max_text_bytes: 4096,
        max_collection_items: 100,
        max_page_items: 25,
        max_operation_bytes: 100000,
    }
}
fn library() -> (SourceLibrary, ProfileSelection) {
    let account = ScopeRef("account".into());
    let mut library = SourceLibrary::empty(account.clone());
    let source = Source {
        source_ref: reference("source"),
        owner_scope: account.clone(),
        provider: Provider::Syndication,
        original_url: "https://example.org/feed".into(),
        display_name: "Example".into(),
        revision: Revision(1),
        paused: false,
        common: Extensions::default(),
    };
    library = library
        .add_source(&account, library.revision, source, &bounds())
        .unwrap();
    for group in ["first", "second"] {
        library = library
            .save_group(
                &account,
                library.revision,
                SourceGroup {
                    group_ref: reference(group),
                    owner_scope: account.clone(),
                    name: group.into(),
                    description: String::new(),
                    keywords: vec![],
                    priority: 0,
                    tag_refs: BTreeSet::new(),
                    revision: Revision(1),
                    common: Extensions::default(),
                },
                &bounds(),
            )
            .unwrap();
        library = library
            .attach(
                &account,
                library.revision,
                &reference(group),
                &reference("source"),
                &bounds(),
            )
            .unwrap();
    }
    let selection = ProfileSelection {
        owner_scope: account,
        profile_ref: reference("profile"),
        selected_groups: [reference("first"), reference("second")].into(),
        selected_sources: BTreeSet::new(),
    };
    let selected = library
        .selected_sources(&selection, &[reference("source")].into())
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(library.acquisition_sources(&selected).len(), 1);
    (library, selection)
}
fn query(library: &SourceLibrary, selection: &ProfileSelection) -> WallQuery {
    WallQuery::from_library(
        library,
        selection,
        Revision(1),
        "example-epoch".into(),
        &[reference("source")].into(),
        [ScopeRef("profile-view".into())].into(),
        None,
        ContextFilter::AllAuthorized,
        WallFilter::default(),
        WallSort::Newest,
        PageSize::Items25,
        &bounds(),
    )
    .unwrap()
}
fn job_recovery() {
    let operation = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    let request = SourceRequest {
        subscription: Subscription {
            subscription_ref: reference("subscription"),
            source_ref: reference("source"),
            partition: AccessPartition {
                scope: ScopeRef("profile-view".into()),
                mode: AccessMode::PublicFeed,
                connection_ref: None,
            },
            executor_ref: reference("executor"),
            generation: Revision(1),
            paused: false,
            common: Extensions::default(),
        },
        operation_id: operation,
        continuation: None,
        bounds: bounds(),
    };
    let mut queue = JobQueue::new(JobLimits {
        max_jobs: 1,
        max_running: 1,
        max_attempts: 2,
        timeout_ticks: 10,
        initial_backoff_ticks: 1,
        max_backoff_ticks: 4,
    })
    .unwrap();
    queue.enqueue(request, 0).unwrap();
    queue.start(operation, &reference("executor"), 0).unwrap();
    let bytes = serde_json::to_vec(&queue).unwrap();
    let mut restarted: JobQueue = serde_json::from_slice(&bytes).unwrap();
    restarted.recover_interrupted(1, true).unwrap();
    assert_eq!(restarted.jobs()[0].state, JobState::AwaitingRecovery);
    assert!(restarted.remove_terminal(operation).is_err());
    assert!(restarted
        .start(operation, &reference("executor"), 2)
        .is_err());
}
fn parsing() {
    let bytes=b"<rss version='2.0'><channel><title>Example</title><link>https://example.org/</link><description>Example</description><item><guid isPermaLink='false'>stable-1</guid><link>https://example.org/paper</link><title>Paper</title><pubDate>Fri, 23 Aug 2024 00:00:00 -0400</pubDate></item></channel></rss>";
    let first = rss_provider_syndication::rss::parse(
        "https://example.org/feed",
        bytes,
        100000,
        10,
        &bounds(),
    )
    .unwrap();
    let second = rss_provider_syndication::rss::parse(
        "https://example.org/feed",
        bytes,
        100000,
        10,
        &bounds(),
    )
    .unwrap();
    assert_eq!(first.entries, second.entries);
    assert_eq!(first.entries[0].key, "stable-1");
    assert_eq!(
        first.entries[0]
            .published_at
            .as_ref()
            .unwrap()
            .offset_minutes,
        Some(-240)
    );
    use rss_provider_syndication::science::arxiv;
    let source = arxiv::source("https://rss.arxiv.org/atom/cs.AI", &bounds(), 10).unwrap();
    let atom=b"<feed xmlns='http://www.w3.org/2005/Atom'><id>http://rss.arxiv.org/atom/</id><title>Daily</title><updated>2026-10-23T00:00:00Z</updated><entry><id>oai:arXiv.org:2408.12345v2</id><title>Paper</title><link href='https://arxiv.org/abs/2408.12345'/><published>2024-08-23T00:00:00-04:00</published></entry></feed>";
    let document = arxiv::parse(&source, 0, atom, 100000, 10, &bounds()).unwrap();
    assert_eq!(
        document.entries[0].reliable_provider_identity(),
        "arxiv/paper/2408.12345"
    );
    assert_eq!(document.entries[0].paper.version, Some(2));
}
#[cfg(feature = "gui")]
fn native_gui(mut session: QuerySession, manager: ManagerModel) {
    struct NoMedia;
    impl rss_gui::MediaPainter for NoMedia {
        fn preview(&mut self, _: &mut egui::Ui, _: egui::Rect, _: &hsscrap::MemberRef) -> bool {
            panic!("empty consumer has no media")
        }
        fn player(&mut self, _: &mut egui::Ui, _: egui::Rect, _: &hsscrap::MemberRef) -> bool {
            panic!("empty consumer has no media")
        }
    }
    let mut wall = WallState::new(bounds()).unwrap();
    let mut view = rss_gui::WallView::new("standalone");
    let mut controls = BTreeSet::new();
    let _ = egui::Context::default().run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            controls.extend(
                view.show(ui, &mut session, &mut wall, &manager, &mut NoMedia)
                    .controls
                    .into_keys(),
            );
        });
    });
    assert!(controls.contains("sources"));
    assert!(controls.contains("refresh"));
    browser::native_check();
}
#[cfg(feature = "providers")]
fn public_provider_calls() {
    assert!(rss_provider_instagram::public::numeric_id(
        "9007199254740993"
    ));
    assert!(matches!(
        rss_provider_youtube::public::resolve_url(
            "https://www.youtube.com/@GoogleDevelopers",
            &bounds()
        )
        .unwrap(),
        rss_provider_youtube::public::SourceTarget::Handle(_)
    ));
    assert!(
        rss_provider_tiktok::public::resolve_url("https://www.tiktok.com/@example", &bounds())
            .is_ok()
    );
    assert!(matches!(
        rss_provider_pinterest::public::resolve_url(
            "https://www.pinterest.com/example/board/",
            &bounds()
        )
        .unwrap(),
        rss_provider_pinterest::public::SourceTarget::Board { .. }
    ));
}
fn main() {
    assert_eq!(CONTRACT_VERSION, "rss.contracts.v1");
    let (library, selection) = library();
    let mut session = QuerySession::new(query(&library, &selection), bounds(), 50).unwrap();
    let ticket = session.load().unwrap();
    let page = WallPage {
        snapshot: WallSnapshot {
            token: "canonical-example".into(),
            query: ticket.request.query.clone(),
        },
        rows: vec![],
        next: None,
        anchor: AnchorResolution::NotRequested,
        health: PageHealth::Ready,
    };
    session.complete(&ticket, Ok(page)).unwrap();
    assert_eq!(session.phase(), &QueryPhase::Empty);
    job_recovery();
    parsing();
    let checks = vec![
        "source_group_profile",
        "query_public_api",
        "job_restart_recovery",
        "rss_science_provenance",
    ];
    #[cfg(feature = "gui")]
    let checks = {
        let manager = ManagerModel::from_library(
            &library,
            &selection,
            &[reference("source")].into(),
            &std::collections::BTreeMap::new(),
            &bounds(),
        )
        .unwrap();
        native_gui(session, manager);
        let mut checks = checks;
        checks.extend(["native_gui_controls", "native_bridge_delegation"]);
        checks
    };
    #[cfg(feature = "providers")]
    let checks = {
        public_provider_calls();
        let mut checks = checks;
        checks.push("public_provider_api_calls");
        checks
    };
    println!(
        "{}",
        json!({"status":"pass","data":"synthetic-independent-consumer","checks":checks,"provider_access":"unproven","host_persistence":"unproven"})
    );
}
