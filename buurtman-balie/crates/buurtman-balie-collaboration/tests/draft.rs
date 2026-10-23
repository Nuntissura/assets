use buurtman_balie_collaboration::{Domain, Draft, Error, Limits};
use std::sync::{Arc, Barrier};
use std::thread;

fn limits() -> Limits {
    Limits::new(4096, 8192, 65536).unwrap()
}
fn domain() -> Domain {
    Domain::new("workspace", "record", "generation").unwrap()
}
fn draft(id: u64) -> Draft {
    Draft::new(domain(), id, limits()).unwrap()
}

#[test]
fn unicode_and_checkpoint_forks_preserve_owned_state() {
    let a = draft(1);
    a.splice_utf8(0, 0, "é🙂e\u{301}").unwrap();
    let checkpoint = a.checkpoint().unwrap();
    assert!(matches!(
        Draft::fork(&checkpoint, 1),
        Err(Error::ReplicaReuse)
    ));
    let b = Draft::fork(&checkpoint.clone(), 2).unwrap();
    assert_eq!(b.text().unwrap(), "é🙂e\u{301}");
    assert!(matches!(b.splice_utf8(1, 0, "x"), Err(Error::InvalidRange)));
    assert!(matches!(
        b.splice_utf16(2, 0, "x"),
        Err(Error::InvalidRange)
    ));
    assert_eq!(b.text().unwrap(), "é🙂e\u{301}");
    b.splice_utf16(1, 2, "★").unwrap();
    assert_eq!(b.text().unwrap(), "é★e\u{301}");
    assert_eq!(a.text().unwrap(), "é🙂e\u{301}");
}

#[test]
fn partitions_reverse_delivery_duplicates_and_concurrent_deletes_converge() {
    let seed = draft(10);
    seed.splice_utf8(0, 0, "abcd").unwrap();
    let checkpoint = seed.checkpoint().unwrap();
    let a = Draft::fork(&checkpoint, 11).unwrap();
    let b = Draft::fork(&checkpoint, 12).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let ba = barrier.clone();
    let aa = a.clone();
    let left = thread::spawn(move || {
        ba.wait();
        let one = aa.splice_utf8(1, 1, "é").unwrap();
        let two = aa.splice_utf8(0, 0, "L").unwrap();
        (one, two)
    });
    let bb = barrier.clone();
    let bc = b.clone();
    let right = thread::spawn(move || {
        bb.wait();
        let one = bc.splice_utf8(1, 2, "🙂").unwrap();
        let two = bc.splice_utf8(0, 0, "R").unwrap();
        (one, two)
    });
    barrier.wait();
    let (a1, a2) = left.join().unwrap();
    let (b1, b2) = right.join().unwrap();
    a.merge(&b2).unwrap();
    a.merge(&b2).unwrap();
    a.merge(&b1).unwrap();
    b.merge(&a2).unwrap();
    b.merge(&a1).unwrap();
    b.merge(&a2).unwrap();
    assert_eq!(a.text().unwrap(), b.text().unwrap());
    let c = Draft::fork(&checkpoint, 13).unwrap();
    for update in [&a2, &b2, &a1, &b1, &b1] {
        c.merge(update).unwrap();
    }
    assert_eq!(a.text().unwrap(), c.text().unwrap());
    let restored = Draft::fork(&c.checkpoint().unwrap(), 14).unwrap();
    assert_eq!(restored.text().unwrap(), c.text().unwrap());
}

#[test]
fn simultaneous_handle_readers_and_writers_keep_every_accepted_insert() {
    let a = draft(20);
    let barrier = Arc::new(Barrier::new(5));
    let mut jobs = Vec::new();
    for _ in 0..3 {
        let handle = a.clone();
        let start = barrier.clone();
        jobs.push(thread::spawn(move || {
            start.wait();
            for _ in 0..32 {
                handle.splice_utf8(0, 0, "x").unwrap();
            }
        }));
    }
    let reader = a.clone();
    let start = barrier.clone();
    jobs.push(thread::spawn(move || {
        start.wait();
        for _ in 0..96 {
            assert!(reader.text().unwrap().chars().all(|c| c == 'x'));
            reader.checkpoint().unwrap();
        }
    }));
    barrier.wait();
    for job in jobs {
        job.join().unwrap();
    }
    assert_eq!(a.text().unwrap(), "x".repeat(96));
}

#[test]
fn rejected_domains_profiles_and_quotas_do_not_change_state_or_future_updates() {
    let a = Draft::new(domain(), 30, Limits::new(3, 1024, 8192).unwrap()).unwrap();
    let b = Draft::new(domain(), 31, Limits::new(3, 1024, 8192).unwrap()).unwrap();
    let own = a.splice_utf8(0, 0, "abc").unwrap();
    let remote = b.splice_utf8(0, 0, "def").unwrap();
    assert_eq!(a.merge(&remote), Err(Error::BudgetExceeded));
    assert!(matches!(
        a.splice_utf8(0, 0, "z"),
        Err(Error::BudgetExceeded)
    ));
    assert!(matches!(
        a.splice_utf8(usize::MAX, 1, ""),
        Err(Error::InvalidRange)
    ));
    let wrong = Draft::new(
        Domain::new("other", "record", "generation").unwrap(),
        32,
        limits(),
    )
    .unwrap();
    assert_eq!(wrong.merge(&own), Err(Error::DomainMismatch));
    assert_eq!(draft(33).merge(&own), Err(Error::LimitsMismatch));
    assert_eq!(a.text().unwrap(), "abc");
    let restored = Draft::fork(&a.checkpoint().unwrap(), 34).unwrap();
    assert_eq!(restored.text().unwrap(), "abc");
    let delete = a.splice_utf8(0, 3, "").unwrap();
    restored.merge(&delete).unwrap();
    assert_eq!(restored.text().unwrap(), "");
}

#[test]
fn encoded_update_and_pending_state_budgets_are_atomic() {
    let tiny = Draft::new(domain(), 40, Limits::new(100, 1, 1000).unwrap()).unwrap();
    assert!(matches!(
        tiny.splice_utf8(0, 0, "a"),
        Err(Error::BudgetExceeded)
    ));
    assert_eq!(tiny.text().unwrap(), "");
    let capped = Draft::new(domain(), 41, Limits::new(100, 1000, 10).unwrap()).unwrap();
    assert!(matches!(
        capped.splice_utf8(0, 0, "a"),
        Err(Error::BudgetExceeded)
    ));
    assert_eq!(capped.text().unwrap(), "");

    let profile = Limits::new(1000, 1000, 120).unwrap();
    let receiver = Draft::new(domain(), 42, profile).unwrap();
    let mut rejected = false;
    for id in 50..80 {
        let source = Draft::new(domain(), id, profile).unwrap();
        source.splice_utf8(0, 0, &"a".repeat(20)).unwrap();
        let orphan = source.splice_utf8(0, 0, &"b".repeat(20)).unwrap();
        let before = receiver.checkpoint().unwrap();
        match receiver.merge(&orphan) {
            Ok(()) => assert!(receiver.has_missing_updates().unwrap()),
            Err(Error::BudgetExceeded) => {
                assert_eq!(receiver.checkpoint().unwrap(), before);
                assert_eq!(receiver.text().unwrap(), "");
                rejected = true;
                break;
            }
            other => panic!("unexpected merge: {other:?}"),
        }
    }
    assert!(
        rejected,
        "pending causal bytes must count against state limits"
    );
}
