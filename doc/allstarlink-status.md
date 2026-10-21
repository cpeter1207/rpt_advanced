# AllStarLink links

`rpt_advanced` can connect a radio node to other AllStarLink nodes over IAX2.
The standalone service handles IAX2 itself. The optional Asterisk adapter uses
ASL3 Asterisk for IAX2 calls. Choose one path for a radio; see the
[installation guide](install.md) and [configuration reference](configuration.md).

## What a link does

- A **transceive** link sends and receives program audio.
- A **monitor** link receives audio from the peer but does not send program audio
  to it.
- A **local-monitor** link receives the peer at this radio but does not pass
  that peer's audio to other linked nodes.

The controller prevents a node from linking directly to itself or making a
duplicate direct link. It also rejects a link when known routes show a loop.
Route information from peers can be missing or out of date, so a displayed
network route is only a guide.

Incoming peers must have a verified node identity. Set `link_allow_nodes` and
`link_deny_nodes` to limit who may connect; a deny entry takes priority. Set
`link_lookup_method` and any directory files to control node lookup. See
[node settings](configuration.md#node-settings) for the exact rules.

## Control links by radio

Send DTMF while using the local receiver. These are the default commands; a
node's `link_command_*` settings can change or disable them.

| DTMF | Action |
| --- | --- |
| `*3<node>#` | Make a transceive link. |
| `*2<node>#` | Make a monitor link. |
| `*75<node>#` | Make a local-monitor link. |
| `*1<node>#` | Disconnect a temporary link. |
| `*813<node>#` | Make a permanent transceive link. |
| `*812<node>#` | Make a permanent monitor link. |
| `*818<node>#` | Make a permanent local-monitor link. |
| `*811<node>#` | Disconnect a manually made permanent link. |
| `*10` | Disconnect all connected temporary links. |
| `*806` | Disconnect and pause permanent links, including configured routes. Temporary links stay connected. |
| `*816` | Resume paused permanent links and configured routes. |
| `*70` | Hear direct-link status. |
| `*72` | Hear the last keyed direct link. |
| `*73` | Hear full direct-link status. |
| `*722` | Hear the local time. |
| `*4<node>` | Select a destination for remote commands. Send `#` to exit. |
| `*82` | Test the local radio keypad. Keep sending test keys, then unkey for the reply. |

For ordinary commands with a node number, `#`, local receiver unkey, or a
three-second pause ends the entry. Node `0` means the last link destination.

For remote commands, send `*4` followed by a valid destination node number.
Selection finishes as soon as the node is recognized and passes access checks.
The prefix and node number stay local. Your next digits travel directly to that
node or through connected relays. Do not add `#` to select the node: it exits
the mode and is not forwarded. Unkeying or pausing keeps the destination selected.
Incoming commands still follow the receiving node's access and admin rules. See
[link command mappings](configuration.md#link-command-mappings) before using
custom DTMF codes. Forced ID, DTMF macros, and autopatch are not provided.

The `*82` keypad test needs no admin unlock. It captures all 16 symbols,
including `*`, `#`, and `A` through `D`, until the local receiver unkeys. Captured
keys do not run commands. The reply reads the first 127 symbols and warns if
more were sent. See [keypad testing](configuration.md#check-a-radio-keypad).

Optional [DTMF regeneration](configuration.md#dtmf-regeneration) plays eligible
link digits addressed to this node on its local transmitter. It is off by
default. Generated tones stay off peer links and can begin while the sending
station is still keyed.

Permanent links retry after a failed call or dropped connection. A temporary
link does not retry on its own. A link made with DTMF is lost when the process
stops. To bring back a permanent link at startup, define it in the
[configuration](configuration.md#configuration-owned-permanent-links-and-replacement-windows).

## Check a link

Status and time replies from local DTMF play on the local RF transmitter after
the receiver unkeys. A command sent by a linked peer replies only to that peer
after it unkeys; it does not key the local transmitter. Replies use speech when
available and Morse as a fallback.

On the Asterisk path, run `rpt_advanced link status <local-node>` in the
Asterisk CLI. `rpt link status <local-node>` is also accepted. The output shows
direct peers, link modes, permanent links, and retained links marked `retrying`
or `paused`. Its route list may be incomplete or stale. The Asterisk CLI also
accepts `rpt_advanced command <local-node> <DTMF>` to send a command through
the normal command rules. These CLI commands are not standalone service
commands.

`rpt_advanced link status <local-node>` also lists configured site meters,
inputs, alarms, outputs, and fans. Readings include age and quality. Output
requests and submitted values are separate from electrical readback.
With the default command map, `*76<selector>#` reads a configured meter or input
and `*77<selector>#` plays a named message. Unkey to hear the reply.
See the [site settings and controls](configuration.md#site-meters-inputs-outputs-alarms-and-fans).

The `fanout` line shows completed audio batches, the longest batch time in
nanoseconds, and dropped audio blocks. Check host load if `dropped-blocks`
keeps rising: some outgoing audio is being lost. The `media` counters are not
used on the Asterisk path.

The `scheduler` line shows failed control tasks and queue delays. If its failure
counts or `delayed_tasks` keep rising, check the host log. Queue waits of one
second or more and control failures produce warnings, limited to one per minute
for each warning type. Reload does not clear the totals.

Standalone service checks use `systemctl status rpt-advanced` and
`journalctl -u rpt-advanced`; see the [installation guide](install.md#set-up-standalone).
The standalone program has no link-status or DTMF command-line option.

This is alpha software. Confirm RF audio, PTT, DTMF, link recovery, and peer
compatibility on the intended equipment before putting a node into service.
