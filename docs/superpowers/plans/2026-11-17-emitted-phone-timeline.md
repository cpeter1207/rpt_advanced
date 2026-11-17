# Emitted phone timeline implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Track the checkboxes below.

**Goal:** Keep strictly verified in-packet adaptive trims transparent without restoring skipped duration.
**Architecture:** Retain wire/native chronology separately from contiguous emitted sink coordinates. Bind trim proof to one retained decision, with original receipt and exact credit.
**Tech Stack:** Rust, released codec/jitter DSOs, native Debian13 quality container.
**Spec:** ../specs/2026-11-17-emitted-phone-timeline-design.md (reviewed SHA2569d459c0f7fe2ffab7f9ea1532989087fac745e5a195cc171982ebb6b8856b9ac).

## Global constraints
- No live node, main, deployment, release or APT changes; no publication in this task.
- Original SIP P assertion stays intact; no timing relaxation or retry-only acceptance.
- Unknown receipts/counter movement/full skips retain cut authority; no new PLC speech or receipt refresh.
- Fresh image; one1CPU1536MiB container at a time, retained cache and RUSTFLAGS="${RUSTFLAGS:-} -Dwarnings"; disk floor2GiB.
- IAX provenance/API parity is a required separate workstream, not solved by RTP changes.

## Review focus
- Partial sink refusal cannot repeat decode/trim or leak wire coordinates.
- Later hard cut must preserve emitted cursor and exact callback custody.
- Host epoch changes cannot relabel old receipt/PCM.
- Delayed/refused symbol-only events and in-band lookback retain command authority.
- Queue measurements must prove removed duration rather than compare adaptive occupancy snapshots.

### Task1: Retained RTP emitted timeline
**Files:** rust/standalone/src/phone/call_media.rs, call_media_tests.rs.
**Interfaces:** Preserve Media::read and existing ABI; separate private wire and emitted cursors, retained decision classification. No provider ABI changes.
- [ ] Preserve current experimental diff and logs in .work/downlink-repro; remove failed gap-spike behavior/flag, retain useful diagnostic tests only as rewritten final tests.
- [ ] Write native failing test asserting proven80trim of160packet emits80suffix at contiguous host start, zero gap/PLC/cut, original receipt, decode once; run and record RED.
- [ ] Implement separate cursors and retained decision proof; all sink audio/gap/adaptation span starts use emitted axis; native witness/counters stay wire-axis.
- [ ] Add zero/17/63 suffix-credit cases, forged/missing evidence, fullskip/cut and wrap/overflow tests. Verify native lease advancement only after all credit.
- [ ] Add trim→partial→cut/replacement, epoch mismatch, delayed/duplicate/refused digits and in-band lookback coverage without inventing timestamp-bearing digit API.
- [ ] Run phone::call_media::tests:: and affected telephone/media tests with warnings denied; fmt/check; recovery commit only after targeted pass.

### Task2: Actual ingress and original SIP behavior
**Files:** rust/product/src/link/phone_media_tests.rs; rust/standalone/src/host_services/product_sip_vox_tests.rs and related existing host test harness if necessary.
**Interfaces:** Actual source sink/Ingress/ring; do not replace real provenance with invented timestamps.
- [ ] Retain negative ring diagnostic showing160/160/80 enqueue deltas and inherited age; add native→product bridge proof with repeated exact trims and paced/stalled consumer.
- [ ] Assert emitted accepted duration reduced by exact trimmed total, bounded age/queue, stable generation, unchanged hold, command/retirement no resurrection.
- [ ] Run original real SIP tests retaining P and cancellation assertions; run product loss/age/authorization regressions and full core courtesy suite.
- [ ] Update ADR0044 and architecture developer docs to final proven boundary, explicitly retaining unresolved IAX qualification work.

### Task3: Qualification and review
- [ ] Run complete pre-push formatting/lint/static-analysis including Cppcheck and Clang-Tidy on frozen tree; distinguish remaining fullPR/coverage gate.
- [ ] Independent review final hash and exact test evidence; fix findings with regression tests.
- [ ] Report exact hashes and hosted qualification publication required to root. No push without instructed authorization; no node changes.
