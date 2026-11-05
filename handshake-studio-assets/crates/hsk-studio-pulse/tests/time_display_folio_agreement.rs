//! Lane SMB-PR-SEQUENCE-SESSION-1, brief item 4: STU-VID-012a's time-display enumeration exists twice,
//! as Pulse's `TimeDisplay` and Folio's `StudioTimeDisplay`, because Folio may depend only on Accord.
//! Pulse depends on Folio, so the agreement is pinned here: every code either side accepts, the member
//! count and the member-to-member pairing, in both directions. Adding a member to either enumeration
//! stops this file compiling until the pairing is written (the two `match`es have no wildcard).
use hsk_studio_folio::StudioTimeDisplay as F;
use hsk_studio_pulse::TimeDisplay as P;

/// The 15 members STU-VID-012a declares; code 113 is a declared gap with no member (STU-VID-012b).
const MEMBERS: usize = 15;

fn folio_of(member: P) -> F {
    match member {
        P::Timecode24 => F::Timecode24,
        P::Timecode25 => F::Timecode25,
        P::Timecode2997Drop => F::Timecode2997Drop,
        P::Timecode2997NonDrop => F::Timecode2997NonDrop,
        P::Timecode30 => F::Timecode30,
        P::Timecode50 => F::Timecode50,
        P::Timecode5994Drop => F::Timecode5994Drop,
        P::Timecode5994NonDrop => F::Timecode5994NonDrop,
        P::Timecode60 => F::Timecode60,
        P::Frames => F::Frames,
        P::Timecode23976 => F::Timecode23976,
        P::FeetFrames16mm => F::FeetFrames16mm,
        P::FeetFrames35mm => F::FeetFrames35mm,
        P::AudioSamples => F::AudioSamples,
        P::Milliseconds => F::Milliseconds,
    }
}

fn pulse_of(member: F) -> P {
    match member {
        F::Timecode24 => P::Timecode24,
        F::Timecode25 => P::Timecode25,
        F::Timecode2997Drop => P::Timecode2997Drop,
        F::Timecode2997NonDrop => P::Timecode2997NonDrop,
        F::Timecode30 => P::Timecode30,
        F::Timecode50 => P::Timecode50,
        F::Timecode5994Drop => P::Timecode5994Drop,
        F::Timecode5994NonDrop => P::Timecode5994NonDrop,
        F::Timecode60 => P::Timecode60,
        F::Frames => P::Frames,
        F::Timecode23976 => P::Timecode23976,
        F::FeetFrames16mm => P::FeetFrames16mm,
        F::FeetFrames35mm => P::FeetFrames35mm,
        F::AudioSamples => P::AudioSamples,
        F::Milliseconds => P::Milliseconds,
    }
}

/// Every disagreement between two code tables (`code -> member code`, for every code either accepts).
fn disagreements(pulse: &[(u16, u16)], folio: &[(u16, u16)]) -> Vec<String> {
    let mut out = Vec::new();
    if pulse.len() != MEMBERS {
        out.push(format!("pulse has {} members, the enumeration has {MEMBERS}", pulse.len()));
    }
    if folio.len() != MEMBERS {
        out.push(format!("folio has {} members, the enumeration has {MEMBERS}", folio.len()));
    }
    for (code, member) in pulse {
        match folio.iter().find(|(c, _)| c == code) {
            None => out.push(format!("code {code}: pulse only")),
            Some((_, other)) if other != member => out.push(format!("code {code}: pulse member {member}, folio member {other}")),
            Some(_) => {}
        }
    }
    for (code, _) in folio {
        if !pulse.iter().any(|(c, _)| c == code) {
            out.push(format!("code {code}: folio only"));
        }
    }
    out
}

/// Every code a side accepts, walked over the whole `u16` range rather than a list of members, with
/// the code the accepted member reports.
fn pulse_table() -> Vec<(u16, u16)> {
    (0..=u16::MAX).filter_map(|code| P::from_code(code).ok().map(|m| (code, m.code()))).collect()
}
fn folio_table() -> Vec<(u16, u16)> {
    (0..=u16::MAX).filter_map(|code| F::from_code(code).map(|m| (code, m.code()))).collect()
}

#[test]
fn pulse_and_folio_accept_the_same_codes_for_the_same_members() {
    let (pulse, folio) = (pulse_table(), folio_table());
    assert_eq!(disagreements(&pulse, &folio), Vec::<String>::new());
    assert!(pulse.iter().chain(&folio).all(|(code, member)| code == member), "a member reports the code it was read from");
    assert_eq!(F::ALL.len(), MEMBERS, "Folio's member list");
}

#[test]
fn every_member_pairs_with_its_counterpart_at_the_same_code() {
    // Folio to Pulse and back, by Folio's own member list.
    for member in F::ALL {
        let pulse = pulse_of(member);
        assert_eq!(pulse.code(), member.code(), "{member:?}");
        assert_eq!(folio_of(pulse), member, "{member:?}");
    }
    // Pulse to Folio and back, by every code Pulse accepts, so a Pulse member Folio's list lacks fails.
    for (code, _) in pulse_table() {
        let pulse = P::from_code(code).unwrap();
        let folio = folio_of(pulse);
        assert_eq!(folio.code(), pulse.code(), "{pulse:?}");
        assert_eq!(pulse_of(folio), pulse, "{pulse:?}");
        assert!(F::ALL.contains(&folio), "{folio:?} is in Folio's list");
    }
    // Code 113 is the declared gap on both sides (STU-VID-012b, gap STU-VID-081).
    assert!(P::from_code(113).is_err());
    assert_eq!(F::from_code(113), None);
    // The audio members are the same on both sides (Folio's `is_audio`, codes 200 and up).
    for member in F::ALL {
        assert_eq!(member.is_audio(), matches!(pulse_of(member), P::AudioSamples | P::Milliseconds), "{member:?}");
    }
}

/// The comparison itself reports each kind of disagreement, so a green run above is not vacuous.
#[test]
fn the_comparison_reports_every_kind_of_disagreement() {
    let agreed = folio_table();
    assert!(disagreements(&agreed, &agreed).is_empty());
    assert_eq!(agreed.len(), MEMBERS);

    let mut short = agreed.clone();
    short.pop();
    let found = disagreements(&short, &agreed);
    assert!(found.iter().any(|d| d.contains("pulse has 14 members")) && found.iter().any(|d| d.contains("folio only")), "{found:?}");
    let found = disagreements(&agreed, &short);
    assert!(found.iter().any(|d| d.contains("folio has 14 members")) && found.iter().any(|d| d.contains("pulse only")), "{found:?}");

    let mut moved = agreed.clone();
    moved[0].1 = 999;
    let found = disagreements(&moved, &agreed);
    assert!(found.iter().any(|d| d.contains("pulse member 999")), "{found:?}");
    let found = disagreements(&agreed, &moved);
    assert!(found.iter().any(|d| d.contains("folio member 999")), "{found:?}");

    let mut extra = agreed.clone();
    extra.push((113, 113));
    let found = disagreements(&extra, &agreed);
    assert!(found.iter().any(|d| d.contains("code 113: pulse only")) && found.iter().any(|d| d.contains("pulse has 16 members")), "{found:?}");
}
