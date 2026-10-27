# Protocol-neutral Codecs and Text Media Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the approved 16 audio-codec profiles and incoming human-text speech without changing unrelated radio behavior.

**Architecture:** Protocol-neutral provider DSOs own codecs; IAX retains negotiation and packet/session behavior. Incoming text has independent forwarding and local-only speech paths. Existing source-rate rings, per-peer owners and callback-generation handoff remain the media boundaries.

**Tech Stack:** Rust 2024, sized C descriptors, released GPLv2-compatible codec libraries, Tokio, existing speech/ring/conversion adapters, Debian 13 packages and GitHub quality images.

**Spec:** [Approved design](../specs/2026-10-27-protocol-neutral-codecs-and-text-media-design.md). Read it and the relevant ADRs before executing either subplan.

## Global Constraints

- Native radio PCM remains normalized mono `f32` at 48 kHz; codec PCM retains its source rate. Audio callbacks remain bounded, lock-free and allocation-free.
- Active formats: `g723`, `gsm`, `ulaw`, `alaw`, `g726aal2`, `adpcm`, `slin`, `lpc10`, `g729`, `speex`, `ilbc`, `g726`, `g722`, `slin16`, `speex16`, `opus`. `siren7`, `siren14`, `g719` remain pending and unadvertised.
- No complex original codecs, relicensing, commercial license purchases, full EchoLink implementation, SIP, REST, new DSP/gains or other wishlist work.
- Only the released inbound ring converts and recovers incoming audio clocks. Preserve and prefer codec-native concealment; the ring conceals only unresolved loss. Egress uses the existing released conversion adapter.
- Text-derived PCM reaches only local RF, never peer audio, parrot or program loopback. Valid text forwarding is independent of synthesis.
- Preserve 160/180/500 ms minimum/initial/maximum packet-jitter settings and existing media age, tail-drain and late-packet behavior.
- Fast formatting/lint/static checks before push; full PR gate on native Debian 13 amd64/arm64, 100% production line/branch coverage on amd64 only. No automatic Debian 12 jobs.
- Developer documentation before implementation commits; operating documentation just before PR. No deployment, RF test, link change or release is authorized by this planning request.

## Review Focus

1. Mismatched provider/host packages fail before a callback starts: integration task I1.
2. Incoming and outgoing calls select the same verified codec composition: codec tasks C5/C8.
3. Encoded bytes, represented PCM duration and media age stay distinct across retries: codec tasks C6/C7.
4. Local speech cannot leak into peer egress even when its sender is also speaking: text task T5.
5. Ordinary reload cannot load a replacement DSO or revive a disconnected text origin: text task T6 and integration task I1.

---

## Execution map

Repository paths below are relative to `C:/Users/ChrisPeterson/Documents/Codex/2026-08-30`:

| Name | Responsibility |
| --- | --- |
| `rptadv-codec-adapters` (new, planned only) | Common C header, neutral profile catalog, provider crates, independent provider packages and tests |
| `librptadviax2` | IAX wire mapping, selection, codec contexts, packet jitter/decode/encode integration |
| `rpt_advanced` | Both hosts, prepared-media metadata, text classification/routing/production/local mix |
| `rptadv-shared-library-workflows` | Reusable provider/IAX gates, prebuilt codec image support |
| `rpt_advanced-workflows` | Product image/package ABI checks and focused-test allowlist |

Use one new provider repository, not a repository per codec. Reuse the existing
shared-library workflow companion; do not create a new workflow engine.

1. [Codec and IAX subplan](2026-10-27-codec-adapters-and-iax-media.md), C1–C8.
2. [Incoming text subplan](2026-10-27-incoming-text-media.md), T1–T6.
3. Integration tasks I1–I2 below.

Text T1–T3 can proceed independently of codec C1–C4. Serialize changes to the
product host table, loader manifests and link lifecycle; C8 establishes host
ABI 8 before text integration. Each subplan has its own focused working tests.
Neither is declared complete on the strength of the other's results.

### Working-tree and evidence preparation

- [ ] Read `AGENTS.md`, `QUALITY.md`, `WISHLIST.md`, architecture README and ADRs listed in spec section 2 in each touched repository. Preserve the baseline for the new provider repository.
- [ ] Record current revisions/diffs in ignored `.work/codec-text-baseline.md`. The planning checkout is `feature/spoken-timeout-recovery`, with an existing spec commit plus dirty timeout work and an unrelated appliance-spec edit. Do not reset, stage or commit these as codec implementation. Reuse a suitable existing clean checkout; if none exists, use the managed-worktree procedure, not an ignored clone.
- [ ] Keep `.work/quality-progress.md` with one row per source/test component: red evidence, focused green command, coverage lines/branches, developer-doc check and commit. Do not rerun full coverage after each fix.
- [ ] Run Linux commands in freshly pulled native `:latest` images through `rpt_advanced-workflows/tools/run-in-quality-container.sh IMAGE WORKSPACE COMMAND...`. Record digest and let the launcher clean its labeled containers on every exit. The existing product image is `ghcr.io/cpeter1207/rpt-advanced-quality-debian13:latest`; C1 adds verified codec prerequisites rather than assuming that image already has them. If Docker is unavailable, use the existing focused GitHub workflow after extending its package allowlist, not the node as a build/test host.

### Coordinated artifact contracts

These revisions are explicit plan decisions, not changes already made:

| Boundary | Revision/action |
| --- | --- |
| Codec-provider descriptor | ABI 1; neutral `rptadv.codec.v1` capability; one independently versioned provider DSO per backend |
| IAX client and server descriptors/options | ABI 2; `rptadv.iax2.v2` / `rptadv.iaxsrv2`; both accept the retained provider catalog |
| IAX library/package | `librptadviax2.so.2`; `librptadv-iax2-client2` and existing development-package name; remove obsolete client1 assumptions |
| IAX async descriptor | Remains ABI 1: runtime/attach/opaque-peer operations are unchanged; client and server handles are created through ABI 2 only |
| Product host services | ABI 8 / `rptadv.hst8` / `rptadv_host_services_v8`; explicit encode consumption, bytes and sample span |
| Product descriptor | ABI 6 / `rptadv.prod6`, rejects old host tables; existing `librptadv_product.so.1` SONAME retained, as descriptor and SONAME already have independent revisions |
| Unchanged components | USBRadioPlus/radio, ring3, samplerate adapter2, packet-jitter adapter1, speech/file adapter1 and control executors keep their existing contracts |

The new codec header owns the neutral encode-result layout, not IAX format
bits. Product and IAX headers include it and generated bindings use the
installed development package. Do not maintain handwritten divergent copies.

### Task I1: Verify the complete dynamically loaded composition

**Files:** Modify `rpt_advanced/tests/test_product_abi.c`, `tests/test_package_combinations.py`, `tests/test_rust_product_surface.py`, `.github/dependencies.json`, `debian/control`; `librptadviax2/tests/abi_header_smoke.c`, `tests/test_install.py`; workflow companion `.github/actions/install-shared-dependencies/action.yml`, `.github/workflows/images.yml`, `.github/workflows/focused-tests.yml`, `tools/test_radio_product_dependencies.py`. Provider package/image inputs are owned by C1.

**Interfaces:** Consumes C8's host8/product6 and C5's client2/server2 descriptors, provider ABI1 and T6's generation retirement. Produces the existing ABI/install checks with the new exact accepted composition; no additional public API.

- [ ] **Red:** Add `test_codec_package_combinations` and `test_codec_dependency_versions` with these assertions:

  ```python
  assert load_set(client=2, server=2, host=8, product=6, codec=1).starts
  assert not load_set(client=1, server=2, host=8, product=6, codec=1).starts
  assert not load_set(client=2, server=1, host=8, product=6, codec=1).starts
  assert not load_set(client=2, server=2, host=7, product=6, codec=1).starts
  assert not load_set(client=2, server=2, host=8, product=5, codec=1).starts
  assert missing_selected_provider().fails_before_audio
  assert ordinary_reload().new_dso_loads == 0
  ```

  `load_set` is a local test helper using staged real DSOs/header fixtures, not a production loader. Exercise a controlled stop/replace/start with a second provider fixture: old contexts retire first and the next start observes the replacement. Verify `readelf` dependencies: external codec libraries only in providers; no Asterisk/PortAudio/ALSA in codec or IAX libraries; no duplicate embedded codec implementation.
- [ ] **Red command:** `python3 -m unittest discover -s tests -p test_package_combinations.py` in product, and `python3 -m unittest discover -s tools -p test_radio_product_dependencies.py` in its workflow companion. Expected: old ABI/package assertions fail. Add corresponding C smoke assertions and run `make product-abi-check` / IAX `make build/abi-header-smoke`.
- [ ] **Implement:** Replace ABI1 IAX fallbacks and stale dependency checks, add released provider packages/headers to the selected manifest and image checks, and rebuild affected prebuilt image manifests. Update `.github/dependencies.json` only with the actual validated dependency revision; never invent a SHA. Runtime mandatory-provider errors must preserve the active configuration. Do not introduce a silent reduced-codec composition.
- [ ] **Green:** Repeat the focused commands and run staged `make install-check` in provider/IAX plus product `make install-check`. Wrong sets fail, the ABI-matched standalone and Asterisk compositions load, and dependent images verify their fresh manifests before use. Record results.
- [ ] **Commit:** Stage only the files listed above and affected package fixtures; commit `build: validate codec-neutral provider and host composition` separately in each repository. No unrelated working-tree changes.

### Task I2: Close verification and documentation without expanding scope

**Files:** Existing test/build/quality inputs in the touched repositories; product `WISHLIST.md`, architecture README, ADRs 0007/0022/0025/0037, and operator manuals/configuration examples immediately before PR; IAX `requirements.md`; provider profile evidence from C1.

**Interfaces:** Consumes the C/T completion matrix and existing quality targets. Produces verified artifact/profile claims and truthful implemented/pending statuses, not new behavior.

- [ ] Add final integration test `codec_text_radio_regressions` covering IDs, courtesy tones, DTMF, timeout/inhibit, source routing, configured gains, text plus same-peer audio, and both call directions. Assert deferred formats are never offered and G.723 5.3 kb/s encoding remains pending. Run the focused case first; expected green with unchanged pre-existing radio semantics.
- [ ] Close only uncovered production source parts from the last coverage report, logging completed 100%/100% parts in `.work/quality-progress.md`. Check affected Rustdoc/C headers with warnings denied; remove unreachable code instead of inventing tests for it.
- [ ] Before push run platform-independent format/lint/static checks once, non-mutating and concurrent where independent. At PR time update concise operating instructions and source/license notices. Push checks remain fast; full native Debian 13 amd64/arm64 PR gates run in parallel with production coverage on amd64 only. Fix failures with focused reruns, then the final gate.
- [ ] Do not merge/release/install automatically: present passed gates and request the next authorized action. A later approved release uses validated main and artifact checks, not another full quality gate. A later node validation must install an ABI-matched set, preserve configuration and obtain RF/listening confirmation before claiming live interoperability.
- [ ] Commit scoped documentation and final tests as `docs: record verified codec and text media support`. Keep deferred/unverified modes and the unimplemented EchoLink protocol visibly pending.

## Plan review record

This is an implementation plan, not a verification result. The author checked
spec coverage, cross-task signatures, task-sized red/green cycles and the five
review-focus cases. No production code, workflows, packages or node state are
changed by preparing it.
