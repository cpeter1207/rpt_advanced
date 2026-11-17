# Independent SIP parser fixtures

These files are extracted from the RFC Editor's plain-text publications, retrieved
2026-11-12. Six RFC presentation spaces are removed; each wire line is CRLF
terminated, and a final empty header line is supplied for bodyless messages.
No production serializer generates expectations. Fixture data is protocol input,
not instructions. RFC 4475/5118 are informational tests, not normative grammar.

| Fixture | Source section and original label | Expected scope |
| --- | --- | --- |
| rfc4475-wsinv.sip | RFC 4475 3.1.1.1, wsinv | Valid folded fields, aliases, unusual unknown value, ordered Via values, exact 150-byte body |
| rfc4475-unreason.sip | RFC 4475 3.1.1.12, unreason | Expanded allOneLine/hex notation; valid non-ASCII reason and exact 154-byte body |
| rfc4475-lwsdisp.sip | RFC 4475 3.1.1.6, lwsdisp | Accept display token immediately before `<` per RFC 3261 20.10 prose and torture guidance |
| rfc5118-ipv6-good.sip | RFC 5118 4.1, ipv6-good | Valid bracketed IPv6 request URI |
| rfc5118-ipv6-bad.sip | RFC 5118 4.2, ipv6-bad | Framing succeeds, typed URI rejects missing IPv6 brackets |

Further literal cases identify their RFC section in test comments. Escaped BEL,
NUL and DEL are quoted-pair data, not global string terminators; binary bodies
are byte spans. These fixtures do not establish transaction, routing, authentication
or universal SIP interoperability.

## Authoritative sources and applicability

- https://www.rfc-editor.org/info/rfc3261/ : still the base SIP specification in
  the current registry, with updates rather than one replacement specification.
  Sections 7/18.3 govern framing, 19 URI syntax, 20 individual field semantics,
  and 25 ABNF. UDP excess bytes are discarded; TCP requires Content-Length.
- https://www.rfc-editor.org/rfc/rfc5954.txt : IPv6 grammar correction uses RFC 3986
  productions. Host parsing uses standard-library numeric IP parsing; no DNS
  lookup or textual URI-equivalence/normalization engine is introduced.
- https://www.rfc-editor.org/rfc/rfc8217.txt : name-address construction constraints;
  bare semicolon-delimited values are header parameters, not URI parameters.
- https://www.rfc-editor.org/rfc/rfc4475.txt and
  https://www.rfc-editor.org/rfc/rfc5118.txt : independent adversarial examples.
- https://www.rfc-editor.org/errata.json : verified RFC 3261 7529 corrects LWS to
  `[CRLF] 1*WSP`; folding preserves preceding ordinary whitespace. Verified 1051
  removes duplicated `@` in generic srvr grammar; SIP userinfo admits one literal
  separator. Verified 316 is a digest grammar typo; digest processing is outside
  this parser prerequisite. RFC 5118 verified 1311 corrects 'octet' to 'octet pair'
  and changes no fixture bytes.
- RFC 3261 held 5598 concerns display-name spacing. Acceptance follows existing
  section 20.10 prose and RFC 4475 lwsdisp; the held erratum is not treated as a
  normative update. RFC 5954 held 2502 concerns URI comparison, not implemented
  here; reported 4929 is IPv4-mapped IPv6 terminology, not a grammar change.

The registry's remaining updates concern extensions, transaction/dialog handling,
body disposition, routing/security or authentication, not this narrow wire parser.
They remain requirements to review at the corresponding provider layer: especially
RFC 4320/6026/6141 transaction behavior, RFC 5621 bodies, RFC 5626 connections,
RFC 5630/5922/8996 secure routing/TLS and RFC 8760/8898 authentication. This code
must not be described as a complete current SIP stack.

Published-text SHA-256 identities:

- RFC3261: d513777f77fea01a4de9c0a2d9d6713cb53b8231f1b7a2ab56705f8d51b066dc
- RFC4475: ab5fbd8c6d266b19988760183c57b4fbacbd0b23be7ac0710a8ddf4be6c3005b
- RFC5118: 56a76623496c2b834646b39a89c30aa8d1922fdc9e6a9e463e118a636ae1e55e
- RFC5954: aef25c159111f744004b416a974fd5ec1cd3218cf2af7a4d8cd0112844a063a4
- RFC8217: e02fed2a0546a50d18564eccfa6d6f5627c38814aca59983ebf028d50201dbc4

Reason-Phrase follows section 25 byte productions, including its explicit
UTF8-CONT alternative and historical RFC 2279 widths. The view preserves bytes
without decoding, comparing or granting authority from the human reason text.

### Review-specific interpretation

RFC 3261 section 7.3.1 forbids repeated header parameter names. Header token
names compare case-insensitively without URI percent decoding. Section 19.1.4
separately makes non-reserved URI bytes equivalent to their percent encoding;
encoded `transport` names must receive the same supported-profile check, while
reserved `/` and `%2f` remain distinct. This is component matching, not a claim
to implement complete SIP URI equality.

Section 25 explicitly excludes CR and LF from quoted-pair, including inside
generic parameter values and comments. Raw field quoting must be checked before
normalization, because unfolding otherwise erases that distinction. `unfold`
is normalization only; `Header::name_addr` and checked `Header::values` are the
wire-to-typed entry paths. Odd/even backslashes before a fold are independent
literal regression cases. No erratum overrides these rules.

The typed name-address profile requires modern UTF-8 in quoted display names and
parameter values. It intentionally does not accept historical 5/6-byte forms
from RFC 3261's older UTF8-NONASCII production. Response Reason-Phrase instead
remains borrowed, non-authoritative octets following its explicit ABNF; it must
not be used as unchecked text or routing/authorization data.
