//! Replay files (ADR 0009): round trips, the version lock, corruption and the
//! writer's limits.

use st_data::{DataSetHash, data_set_hash};
use st_rng::Stream;
use st_sim::replay::{
    MAGIC, PlayContext, ReplayError, ReplayHeader, ReplayReader, ReplayWriter, read_header,
};
use st_sim::scenarios::{self, Scenario};
use st_sim::{CHECKPOINT_INTERVAL, Command, SIM_VERSION, Sim, StateHasher, command};

fn empty_data_set() -> DataSetHash {
    data_set_hash(&[]).unwrap()
}

fn header_for(s: &Scenario) -> ReplayHeader {
    ReplayHeader::new(&empty_data_set(), s.seed, "test build 1.2.3", vec![1])
}

/// Records the first `ticks` ticks of a scenario through the writer, taking
/// bytes at every checkpoint as a session would.
fn record_with(s: &Scenario, ticks: u64, header: &ReplayHeader) -> Vec<u8> {
    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut writer = ReplayWriter::new(header, &s.setup, sim.state_hash().unwrap()).unwrap();
    let mut file = Vec::new();
    while sim.tick() < ticks {
        let tick = sim.tick();
        let mut now: Vec<Command> = s
            .commands
            .iter()
            .filter(|c| c.tick == tick)
            .cloned()
            .collect();
        command::sort(&mut now);
        sim.step(&now).unwrap();
        writer.push_tick(tick, &now).unwrap();
        if sim.tick().is_multiple_of(CHECKPOINT_INTERVAL) {
            writer
                .checkpoint(sim.tick(), sim.state_hash().unwrap())
                .unwrap();
            file.extend(writer.take_bytes());
        }
    }
    file.extend(
        writer
            .finish(sim.tick(), sim.state_hash().unwrap())
            .unwrap(),
    );
    file
}

fn record(s: &Scenario, ticks: u64) -> Vec<u8> {
    record_with(s, ticks, &header_for(s))
}

fn ctx() -> PlayContext {
    PlayContext::current(empty_data_set())
}

/// A chunk with a valid check.
fn chunk(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(tag);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    let mut h = StateHasher::new();
    h.bytes(&out);
    out.extend_from_slice(&h.finish().to_le_bytes());
    out
}

/// Recomputes the check of every chunk whose frame fits in the bytes, so a
/// mutation reaches the decoders behind the checks.
fn reseal(bytes: &mut [u8]) {
    let mut pos = MAGIC.len();
    while pos + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8 + len;
        if body + 8 > bytes.len() {
            return;
        }
        let mut h = StateHasher::new();
        h.bytes(&bytes[pos..body]);
        bytes[body..body + 8].copy_from_slice(&h.finish().to_le_bytes());
        pos = body + 8;
    }
}

/// The offsets of the chunks: (tag, start, end).
fn chunks(bytes: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut pos = MAGIC.len();
    while pos + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let end = pos + 8 + len + 8;
        out.push((bytes[pos..pos + 4].try_into().unwrap(), pos, end));
        pos = end;
    }
    out
}

#[test]
fn every_scenario_records_and_verifies() {
    for s in scenarios::all() {
        let bytes = record(&s, s.ticks);
        let reader = ReplayReader::open(&bytes, &ctx()).unwrap();
        let verified = reader.verify().unwrap();
        assert_eq!(verified.final_tick, s.ticks, "{}", s.name);
        assert!(verified.complete);
        let (tick, hash) = reader.end().unwrap();
        assert_eq!((tick, hash), (verified.final_tick, verified.final_hash));
        assert_eq!(hash, s.run().unwrap().last().unwrap().hash, "{}", s.name);
        // The decoded commands are the scenario's, bit for bit.
        let mut decoded = Vec::new();
        for i in 0..reader.segment_count() {
            for record in reader.segment(i).unwrap().records().unwrap() {
                decoded.extend(record.commands);
            }
        }
        assert_eq!(decoded, s.commands, "{}", s.name);
    }
}

#[test]
fn recording_is_canonical_and_repeatable() {
    let s = scenarios::swarm_controls();
    let a = record(&s, 600);
    assert_eq!(a, record(&s, 600));
    // Read and write again: the bytes are identical.
    let reader = ReplayReader::open(&a, &ctx()).unwrap();
    let mut sim = reader.new_sim().unwrap();
    let mut writer =
        ReplayWriter::new(reader.header(), reader.setup(), reader.start_hash()).unwrap();
    let mut file = Vec::new();
    for i in 0..reader.segment_count() {
        let segment = reader.segment(i).unwrap();
        let mut records = segment.records().unwrap().into_iter().peekable();
        for _ in 0..segment.tick_count {
            let tick = sim.tick();
            let now = match records.next_if(|r| r.tick == tick) {
                Some(r) => r.commands,
                None => Vec::new(),
            };
            sim.step(&now).unwrap();
            writer.push_tick(tick, &now).unwrap();
        }
        let hash = sim.state_hash().unwrap();
        if sim.tick().is_multiple_of(CHECKPOINT_INTERVAL) && i + 1 < reader.segment_count() {
            writer.checkpoint(sim.tick(), hash).unwrap();
            file.extend(writer.take_bytes());
        }
    }
    file.extend(
        writer
            .finish(sim.tick(), sim.state_hash().unwrap())
            .unwrap(),
    );
    assert_eq!(file, a);
}

#[test]
fn the_header_is_readable_text_and_read_header_needs_no_body() {
    let s = scenarios::solo_burn();
    let bytes = record(&s, 130);
    let header = read_header(&bytes).unwrap();
    assert_eq!(header, header_for(&s));
    assert_eq!(header.sim_version, SIM_VERSION);
    let (_, start, end) = chunks(&bytes)[0];
    let text = std::str::from_utf8(&bytes[start + 8..end - 8]).unwrap();
    assert!(text.contains("kind: \"replay\""), "{text}");
    assert!(text.contains("sim_version: "), "{text}");
    assert!(!text.contains('\r'));
    // The header alone, cut off right after the HEAD chunk, still reads.
    assert_eq!(read_header(&bytes[..end]).unwrap(), header);
}

#[test]
fn streaming_equals_one_shot() {
    let s = scenarios::orbit_coast();
    let streamed = record(&s, 500);
    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut writer =
        ReplayWriter::new(&header_for(&s), &s.setup, sim.state_hash().unwrap()).unwrap();
    while sim.tick() < 500 {
        let tick = sim.tick();
        let mut now: Vec<Command> = s
            .commands
            .iter()
            .filter(|c| c.tick == tick)
            .cloned()
            .collect();
        command::sort(&mut now);
        sim.step(&now).unwrap();
        writer.push_tick(tick, &now).unwrap();
        if sim.tick().is_multiple_of(CHECKPOINT_INTERVAL) {
            writer
                .checkpoint(sim.tick(), sim.state_hash().unwrap())
                .unwrap();
        }
    }
    let one_shot = writer.finish(500, sim.state_hash().unwrap()).unwrap();
    assert_eq!(streamed, one_shot);
}

// ---- The version lock -------------------------------------------------------

#[test]
fn another_sim_version_is_refused_with_a_clear_message() {
    let s = scenarios::solo_burn();
    let mut header = header_for(&s);
    header.sim_version = SIM_VERSION + 6;
    header.build = "spacetime-quasar 0.3.0".to_owned();
    let bytes = record_with(&s, 130, &header);
    let err = ReplayReader::open(&bytes, &ctx()).unwrap_err();
    assert_eq!(
        err,
        ReplayError::SimVersionMismatch {
            recorded: SIM_VERSION + 6,
            recorded_build: "spacetime-quasar 0.3.0".to_owned(),
            current: SIM_VERSION,
        }
    );
    assert_eq!(
        err.to_string(),
        format!(
            "Recorded with spacetime-quasar 0.3.0 (simulation version {}). \
             This build plays simulation version {SIM_VERSION} only.",
            SIM_VERSION + 6
        )
    );
    // The header still reads, so a tool can say what the file is.
    assert_eq!(read_header(&bytes).unwrap().sim_version, SIM_VERSION + 6);
    // An older version is refused the same way.
    if SIM_VERSION > 1 {
        header.sim_version = SIM_VERSION - 1;
        let bytes = record_with(&s, 130, &header);
        assert!(matches!(
            ReplayReader::open(&bytes, &ctx()),
            Err(ReplayError::SimVersionMismatch { .. })
        ));
    }
}

#[test]
fn another_hash_format_or_data_set_is_refused() {
    let s = scenarios::solo_burn();
    let mut header = header_for(&s);
    header.state_hash_version = 2;
    let bytes = record_with(&s, 130, &header);
    assert!(matches!(
        ReplayReader::open(&bytes, &ctx()),
        Err(ReplayError::HashFormatMismatch {
            recorded: 2,
            current: 1
        })
    ));

    let bytes = record(&s, 130);
    let other = DataSetHash::from_bytes([7; 32]);
    let err = ReplayReader::open(&bytes, &PlayContext::current(other)).unwrap_err();
    match &err {
        ReplayError::DataSetMismatch { recorded, current } => {
            assert_eq!(recorded, &empty_data_set().to_string());
            assert_eq!(current, &other.to_string());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn force_skips_the_version_checks_only() {
    let s = scenarios::solo_burn();
    let mut header = header_for(&s);
    header.sim_version = SIM_VERSION + 1;
    header.state_hash_version = 9;
    let bytes = record_with(&s, 250, &header);
    let mut forced = ctx();
    forced.check_versions = false;
    let reader = ReplayReader::open(&bytes, &forced).unwrap();
    assert_eq!(reader.verify().unwrap().final_tick, 250);
    // A damaged file is still refused.
    let mut damaged = bytes.clone();
    let n = damaged.len();
    damaged[n - 12] ^= 1;
    assert!(ReplayReader::open(&damaged, &forced).is_err());
}

#[test]
fn a_newer_header_version_says_so() {
    let s = scenarios::solo_burn();
    let bytes = record(&s, 130);
    let (_, start, end) = chunks(&bytes)[0];
    let text = std::str::from_utf8(&bytes[start + 8..end - 8]).unwrap();
    let newer = text.replacen("version: 1", "version: 2", 1);
    let mut file = bytes[..start].to_vec();
    file.extend(chunk(b"HEAD", newer.as_bytes()));
    file.extend_from_slice(&bytes[end..]);
    let err = ReplayReader::open(&file, &ctx()).unwrap_err();
    assert_eq!(
        err,
        ReplayError::NewerBuild {
            header_version: 2,
            supported: 1
        }
    );
    assert!(err.to_string().contains("newer build"));
    // Other header faults are diagnostics.
    for bad in [
        text.replacen("kind: \"replay\"", "kind: \"save\"", 1),
        text.replacen("seed:", "sede:", 1),
        text.replacen("b3:", "b4:", 1),
    ] {
        let mut file = bytes[..start].to_vec();
        file.extend(chunk(b"HEAD", bad.as_bytes()));
        file.extend_from_slice(&bytes[end..]);
        assert!(matches!(
            ReplayReader::open(&file, &ctx()),
            Err(ReplayError::Header(_))
        ));
    }
}

// ---- Errors and corruption --------------------------------------------------

#[test]
fn not_a_replay() {
    for bytes in [&b""[..], b"hello", b"\x89STQR\r\n\x1b........", &[0; 100]] {
        assert_eq!(
            ReplayReader::open(bytes, &ctx()).unwrap_err(),
            ReplayError::NotAReplay,
            "{bytes:?}"
        );
    }
    // A prefix of the magic, or shorter than it, is not a replay either.
    for n in 0..MAGIC.len() {
        assert_eq!(
            ReplayReader::open(&MAGIC[..n], &ctx()).unwrap_err(),
            ReplayError::NotAReplay,
            "{n} bytes"
        );
        assert_eq!(
            read_header(&MAGIC[..n]).unwrap_err(),
            ReplayError::NotAReplay
        );
    }
}

#[test]
fn the_writer_refuses_a_header_the_reader_would_refuse() {
    let s = scenarios::solo_burn();
    let hash = Sim::new(&s.setup, s.seed).unwrap().state_hash().unwrap();
    let good = header_for(&s);
    assert!(ReplayWriter::new(&good, &s.setup, hash).is_ok());
    let mut bad_hash = good.clone();
    bad_hash.data_set_hash = "b3:nothex".to_owned();
    let mut long_build = good.clone();
    long_build.build = "x".repeat(257);
    let mut many_pilots = good.clone();
    many_pilots.local_pilots = (0..1025).collect();
    for header in [bad_hash, long_build, many_pilots] {
        assert!(matches!(
            ReplayWriter::new(&header, &s.setup, hash),
            Err(ReplayError::HeaderWrite(_))
        ));
    }
}

#[test]
fn the_size_limit_applies_to_the_reader() {
    let bytes = record(&scenarios::solo_burn(), 130);
    let mut small = ctx();
    small.max_bytes = bytes.len() - 1;
    assert!(matches!(
        ReplayReader::open(&bytes, &small),
        Err(ReplayError::TooLarge { .. })
    ));
    small.max_bytes = bytes.len();
    assert!(ReplayReader::open(&bytes, &small).is_ok());
}

#[test]
fn a_flipped_bit_is_corruption_never_a_desync() {
    let bytes = record(&scenarios::solo_burn(), 250);
    for i in MAGIC.len()..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[i] ^= 0x10;
        match ReplayReader::open(&damaged, &ctx()).and_then(|r| r.verify()) {
            Err(ReplayError::Desync { .. }) => panic!("byte {i} gave a desync"),
            Err(_) => {}
            // A flipped bit in a length can make a chunk run past the end of
            // the file, which reads as a cut-off recording.
            Ok(v) => assert!(!v.complete, "byte {i} went unnoticed"),
        }
    }
}

#[test]
fn a_wrong_end_hash_with_a_good_check_is_a_desync() {
    let s = scenarios::solo_burn();
    let mut bytes = record(&s, 250);
    // Change the end hash of the first SEGM (the last 8 bytes of its payload)
    // and reseal: the file is intact but the playback cannot match it.
    let (_, _, end) = chunks(&bytes)
        .into_iter()
        .find(|(tag, _, _)| tag == b"SEGM")
        .unwrap();
    bytes[end - 16] ^= 1;
    reseal(&mut bytes);
    let reader = ReplayReader::open(&bytes, &ctx()).unwrap();
    match reader.verify().unwrap_err() {
        ReplayError::Desync {
            checkpoint_tick,
            last_good_tick,
            expected,
            actual,
        } => {
            assert_eq!((checkpoint_tick, last_good_tick), (120, Some(0)));
            assert_ne!(expected, actual);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_wrong_start_hash_is_a_desync_at_the_start() {
    let s = scenarios::solo_burn();
    let mut bytes = record(&s, 130);
    let (_, start, _) = chunks(&bytes)[1];
    bytes[start + 8 + 8] ^= 1; // SETP: seed, then start_hash
    reseal(&mut bytes);
    assert!(matches!(
        ReplayReader::open(&bytes, &ctx()),
        Err(ReplayError::Desync {
            checkpoint_tick: 0,
            last_good_tick: None,
            ..
        })
    ));
}

#[test]
fn the_seed_copies_must_agree() {
    let s = scenarios::solo_burn();
    let mut bytes = record(&s, 130);
    let (_, start, _) = chunks(&bytes)[1];
    bytes[start + 8] ^= 1;
    reseal(&mut bytes);
    assert!(matches!(
        ReplayReader::open(&bytes, &ctx()),
        Err(ReplayError::Corrupt { .. })
    ));
}

#[test]
fn trailing_bytes_and_stray_chunks_are_malformed() {
    let bytes = record(&scenarios::solo_burn(), 250);
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        ReplayReader::open(&trailing, &ctx()),
        Err(ReplayError::Malformed { .. })
    ));
    let (_, end_start, _) = *chunks(&bytes).last().unwrap();
    let mut stray = bytes[..end_start].to_vec();
    stray.extend(chunk(b"JUNK", b"x"));
    stray.extend_from_slice(&bytes[end_start..]);
    assert!(matches!(
        ReplayReader::open(&stray, &ctx()),
        Err(ReplayError::Malformed { .. })
    ));
    // Segments out of order: swap the first two segments of a longer file.
    let long = record(&scenarios::solo_burn(), 600);
    let c = chunks(&long);
    let segs: Vec<_> = c.iter().filter(|(t, _, _)| t == b"SEGM").collect();
    let mut swapped = long[..segs[0].1].to_vec();
    swapped.extend_from_slice(&long[segs[1].1..segs[1].2]);
    swapped.extend_from_slice(&long[segs[0].1..segs[0].2]);
    swapped.extend_from_slice(&long[segs[1].2..]);
    assert!(matches!(
        ReplayReader::open(&swapped, &ctx()),
        Err(ReplayError::Malformed { .. })
    ));
}

#[test]
fn a_truncated_file_plays_up_to_its_last_complete_segment() {
    let s = scenarios::solo_burn();
    let bytes = record(&s, 600);
    let c = chunks(&bytes);
    let segs: Vec<_> = c.iter().filter(|(t, _, _)| t == b"SEGM").collect();
    // Cut after the third segment, without END_.
    let cut = &bytes[..segs[2].2];
    let reader = ReplayReader::open(cut, &ctx()).unwrap();
    assert_eq!(
        (reader.segment_count(), reader.truncated_at()),
        (3, Some(360))
    );
    assert_eq!(reader.end(), None);
    let verified = reader.verify().unwrap();
    assert_eq!((verified.final_tick, verified.complete), (360, false));
    assert_eq!(
        reader.segment(3).unwrap_err(),
        ReplayError::Truncated {
            last_complete_tick: Some(360)
        }
    );
    assert_eq!(
        reader.segment(4).unwrap_err(),
        ReplayError::NoSuchSegment(4)
    );
    // Cut in the middle of the fourth segment: the same.
    let mid = (segs[3].1 + segs[3].2) / 2;
    let reader = ReplayReader::open(&bytes[..mid], &ctx()).unwrap();
    assert_eq!(reader.truncated_at(), Some(360));
    // Before the setup is complete there is nothing to play.
    assert!(matches!(
        ReplayReader::open(&bytes[..c[1].1 + 5], &ctx()),
        Err(ReplayError::Truncated {
            last_complete_tick: None
        })
    ));
}

#[test]
fn truncation_at_every_byte_offset_never_panics() {
    let bytes = record(&scenarios::solo_burn(), 250);
    let mut opened = 0;
    for n in 0..=bytes.len() {
        if let Ok(reader) = ReplayReader::open(&bytes[..n], &ctx()) {
            opened += 1;
            // What it holds plays.
            reader.verify().unwrap();
        }
    }
    // The cuts after SETP open: every offset from there to the end.
    let setp_end = chunks(&bytes)[1].2;
    assert_eq!(opened, bytes.len() - setp_end + 1);
}

/// A fixed-seed mutation loop (no new dependency): 10,000 mutations of a
/// valid file, a third of them with the checks recomputed so the decoders
/// behind the checks see them. Each must return an error or a valid log and
/// never panic.
#[test]
fn ten_thousand_seeded_mutations_never_panic() {
    let bytes = record(&scenarios::solo_burn(), 250);
    let stream = Stream::from_seed(0xfeed).derive_label("replay-fuzz");
    let (mut errors, mut valid) = (0_u32, 0_u32);
    for i in 0..10_000_u64 {
        let s = stream.derive(i);
        let mut m = bytes.clone();
        // One to four edits of a random kind.
        for e in 0..=s.below_at(0, 4) {
            let at = s.below_at(10 + 4 * e, m.len() as u64) as usize;
            match s.below_at(11 + 4 * e, 5) {
                0 => m[at] ^= 1 << s.below_at(12 + 4 * e, 8),
                1 => m[at] = s.below_at(12 + 4 * e, 256) as u8,
                2 => {
                    m.remove(at);
                }
                3 => m.insert(at, s.below_at(12 + 4 * e, 256) as u8),
                _ => m.truncate(at.max(1)),
            }
            if m.is_empty() {
                break;
            }
        }
        if i % 3 == 0 {
            reseal(&mut m);
        }
        match ReplayReader::open(&m, &ctx()).and_then(|r| r.verify()) {
            Ok(_) => valid += 1,
            Err(e) => {
                errors += 1;
                // Every error can be shown to a person.
                assert!(!e.to_string().is_empty());
            }
        }
    }
    assert!(errors > 9_000, "{errors} errors, {valid} valid");
}

// ---- The writer ---------------------------------------------------------------

#[test]
fn the_writer_refuses_input_it_cannot_record() {
    let s = scenarios::solo_burn();
    let sim = Sim::new(&s.setup, s.seed).unwrap();
    let new = || ReplayWriter::new(&header_for(&s), &s.setup, sim.state_hash().unwrap()).unwrap();
    let mut header = header_for(&s);
    header.start_tick = 5;
    assert!(ReplayWriter::new(&header, &s.setup, 0).is_err());

    let cmd = |tick, pilot, seq| Command {
        tick,
        pilot: command::PilotId(pilot),
        seq,
        kind: command::CommandKind::Controls(command::ControlState::default()),
    };
    let mut w = new();
    w.push_tick(3, &[cmd(3, 1, 1)]).unwrap();
    // Not in order, a wrong tick, unsorted, a stale seq, seq 0, too many.
    for (tick, commands) in [
        (3, vec![cmd(3, 1, 2)]),
        (2, vec![cmd(2, 1, 2)]),
        (4, vec![cmd(5, 1, 2)]),
        (4, vec![cmd(4, 2, 1), cmd(4, 1, 2)]),
        (4, vec![cmd(4, 1, 1)]),
        (4, vec![cmd(4, 3, 0)]),
        (4, (0..1025).map(|i| cmd(4, 10 + i, 1)).collect()),
    ] {
        assert!(
            matches!(w.push_tick(tick, &commands), Err(ReplayError::BadInput(_))),
            "{tick} {commands:?}"
        );
    }
    // Nothing was recorded by the failures: the writer still works.
    w.push_tick(4, &[cmd(4, 1, 2)]).unwrap();
    w.push_tick(5, &[]).unwrap();
    // Checkpoints are at multiples of 120 and above the segment start.
    assert!(w.checkpoint(60, 0).is_err());
    assert!(w.checkpoint(0, 0).is_err());
    assert!(w.finish(0, 0).is_err()); // final tick before a recorded tick
}

#[test]
fn a_full_file_ends_at_the_previous_checkpoint_and_plays() {
    let s = scenarios::swarm_controls();
    let full = record(&s, 480);
    // The size of the file after three segments, plus half of the fourth.
    let c = chunks(&full);
    let segs: Vec<_> = c.iter().filter(|(t, _, _)| t == b"SEGM").collect();
    let limit = segs[3].1 + (segs[3].2 - segs[3].1) / 2;

    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut writer =
        ReplayWriter::with_limit(&header_for(&s), &s.setup, sim.state_hash().unwrap(), limit)
            .unwrap();
    let mut file = Vec::new();
    let mut last_good = (0, sim.state_hash().unwrap());
    let mut recording = true;
    while sim.tick() < 480 {
        let tick = sim.tick();
        let mut now: Vec<Command> = s
            .commands
            .iter()
            .filter(|c| c.tick == tick)
            .cloned()
            .collect();
        command::sort(&mut now);
        sim.step(&now).unwrap();
        if !recording {
            continue;
        }
        writer.push_tick(tick, &now).unwrap();
        if sim.tick().is_multiple_of(CHECKPOINT_INTERVAL) {
            let hash = sim.state_hash().unwrap();
            match writer.checkpoint(sim.tick(), hash) {
                Ok(()) => {
                    last_good = (sim.tick(), hash);
                    file.extend(writer.take_bytes());
                }
                Err(ReplayError::TooLarge { len, limit: l }) => {
                    assert!(len > l);
                    recording = false;
                }
                Err(e) => panic!("{e}"),
            }
        }
    }
    assert!(!recording);
    file.extend(writer.finish(last_good.0, last_good.1).unwrap());
    assert!(file.len() <= limit);
    let reader = ReplayReader::open(&file, &ctx()).unwrap();
    let verified = reader.verify().unwrap();
    assert_eq!((verified.final_tick, verified.complete), (360, true));
}

#[test]
fn a_segment_that_cannot_fit_at_finish_ends_the_file_at_the_last_checkpoint() {
    let s = scenarios::swarm_controls();
    let sim_hash = |ticks| {
        s.run_with_interval(1)
            .unwrap()
            .into_iter()
            .find(|c| c.tick == ticks)
            .unwrap()
            .hash
    };
    let full = record(&s, 130);
    let c = chunks(&full);
    let segs: Vec<_> = c.iter().filter(|(t, _, _)| t == b"SEGM").collect();
    // Room for the first segment only.
    let limit = segs[0].2 + 40;
    let mut sim = Sim::new(&s.setup, s.seed).unwrap();
    let mut writer =
        ReplayWriter::with_limit(&header_for(&s), &s.setup, sim.state_hash().unwrap(), limit)
            .unwrap();
    while sim.tick() < 130 {
        let tick = sim.tick();
        let mut now: Vec<Command> = s
            .commands
            .iter()
            .filter(|c| c.tick == tick)
            .cloned()
            .collect();
        command::sort(&mut now);
        sim.step(&now).unwrap();
        writer.push_tick(tick, &now).unwrap();
        if sim.tick() == 120 {
            writer.checkpoint(120, sim_hash(120)).unwrap();
        }
    }
    let file = writer.finish(130, sim_hash(130)).unwrap();
    let verified = ReplayReader::open(&file, &ctx()).unwrap().verify().unwrap();
    assert_eq!((verified.final_tick, verified.complete), (120, true));
}

// ---- Segments the format does not allow ---------------------------------------

/// A one-segment file (100 ticks of `solo-burn`) whose `SEGM` payload is
/// replaced by `fields`, keeping its end hash so that `END_` still agrees.
fn with_segment(first_tick: u64, tick_count: u32, records: &[u8], count: u8) -> Vec<u8> {
    let bytes = record(&scenarios::solo_burn(), 100);
    let (_, start, end) = chunks(&bytes)
        .into_iter()
        .find(|(tag, _, _)| tag == b"SEGM")
        .unwrap();
    let end_hash = &bytes[end - 16..end - 8];
    let mut payload = Vec::new();
    payload.extend_from_slice(&first_tick.to_le_bytes());
    payload.extend_from_slice(&tick_count.to_le_bytes());
    payload.push(count);
    payload.extend_from_slice(records);
    payload.extend_from_slice(end_hash);
    let mut file = bytes[..start].to_vec();
    file.extend(chunk(b"SEGM", &payload));
    file.extend_from_slice(&bytes[end..]);
    file
}

/// One `Controls` command frame with default controls (11-byte payload).
fn frame(pilot: u8, seq_field: u8) -> Vec<u8> {
    let mut f = vec![pilot, seq_field, 1, 11];
    f.extend_from_slice(&[0; 11]);
    f
}

fn record_bytes(delta: u8, frames: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![delta, frames.len() as u8];
    for f in frames {
        r.extend_from_slice(f);
    }
    r
}

fn segment_error(file: &[u8]) -> Option<ReplayError> {
    ReplayReader::open(file, &ctx())
        .and_then(|r| r.segment(0)?.records().map(|_| ()))
        .err()
}

#[test]
fn the_format_rules_for_segments_are_enforced() {
    // The honest segment decodes (a sanity check of the helper).
    let good = [
        record_bytes(0, &[frame(1, 1)]),
        record_bytes(5, &[frame(1, 1), frame(2, 1)]),
    ]
    .concat();
    assert_eq!(segment_error(&with_segment(0, 100, &good, 2)), None);

    let malformed = |file: Vec<u8>, what: &str| {
        assert!(
            matches!(segment_error(&file), Some(ReplayError::Malformed { .. })),
            "{what}: {:?}",
            segment_error(&file)
        );
    };
    // The same pilot twice in a tick with a sequence difference of 0, and a
    // first sequence number of 0.
    malformed(
        with_segment(0, 100, &record_bytes(0, &[frame(1, 1), frame(1, 0)]), 1),
        "seq difference 0",
    );
    malformed(
        with_segment(0, 100, &record_bytes(0, &[frame(1, 0)]), 1),
        "seq 0",
    );
    // Pilots out of order within a tick.
    malformed(
        with_segment(0, 100, &record_bytes(0, &[frame(2, 1), frame(1, 1)]), 1),
        "unsorted pilots",
    );
    // A tick delta of 0 after the first record, and a record past the segment.
    let twice = [
        record_bytes(0, &[frame(1, 1)]),
        record_bytes(0, &[frame(1, 1)]),
    ]
    .concat();
    malformed(with_segment(0, 100, &twice, 2), "delta 0");
    malformed(
        with_segment(0, 100, &record_bytes(100, &[frame(1, 1)]), 1),
        "record past the end",
    );
    // A record with no commands, an unknown tag, a reserved tag, trailing bytes,
    // more records than ticks.
    malformed(with_segment(0, 100, &[0, 0], 1), "no commands");
    let mut unknown = frame(1, 1);
    unknown[2] = 7;
    malformed(
        with_segment(0, 100, &record_bytes(0, &[unknown]), 1),
        "unknown tag",
    );
    let reserved = vec![1, 1, 0xe8, 0x07, 0]; // tag 1000 (AddPilot), no payload
    malformed(
        with_segment(0, 100, &record_bytes(0, &[reserved]), 1),
        "reserved tag",
    );
    let mut trailing = record_bytes(0, &[frame(1, 1)]);
    trailing.push(0);
    malformed(with_segment(0, 100, &trailing, 1), "trailing byte");
    malformed(with_segment(0, 100, &[], 101), "more records than ticks");

    // Segment fields: too many ticks, a gap before it, zero ticks.
    for (first, count) in [(0, 121), (1, 99), (0, 0)] {
        malformed(
            with_segment(first, count, &record_bytes(0, &[frame(1, 1)]), 1),
            "segment fields",
        );
    }
}

/// A replay carries its setup, so a hostile file reaches `Sim::new`. It must be refused
/// when the replay is opened, never panic inside Rapier.
#[test]
fn a_replay_with_a_hostile_setup_is_refused_never_a_panic() {
    use st_sim::SetupError;
    let base = scenarios::solo_burn();
    let hostile: Vec<(Box<dyn Fn(&mut st_sim::Setup)>, SetupError)> = vec![
        (
            Box::new(|s| s.ships[0].drag = -1.0e5),
            SetupError::BadDynamics(1),
        ),
        (
            Box::new(|s| s.ships[0].drag = -f64::MAX),
            SetupError::BadDynamics(1),
        ),
        (
            Box::new(|s| s.ships[0].max_accel = -1.0),
            SetupError::BadDynamics(1),
        ),
        (
            Box::new(|s| s.ships[0].max_turn_rate = -1.0),
            SetupError::BadDynamics(1),
        ),
        (Box::new(|s| s.max_speed_mps = 0.0), SetupError::BadMaxSpeed),
        (
            Box::new(|s| s.max_speed_mps = -5.0),
            SetupError::BadMaxSpeed,
        ),
        (Box::new(|s| s.ai.standoff = 0.0), SetupError::BadAiParams),
        (Box::new(|s| s.ai.drift_ticks = 0), SetupError::BadAiParams),
    ];
    for (change, expected) in hostile {
        let mut setup = base.setup.clone();
        change(&mut setup);
        let bytes = ReplayWriter::new(&header_for(&base), &setup, 0)
            .unwrap()
            .finish(0, 0)
            .unwrap();
        // `open` builds the simulation, so the refusal comes there.
        assert!(
            matches!(ReplayReader::open(&bytes, &ctx()), Err(ReplayError::Setup(e)) if e == expected)
        );
    }
}
