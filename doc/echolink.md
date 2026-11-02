# EchoLink links

EchoLink links carry voice and text between your radio node and EchoLink
stations. Use a separate validated EchoLink account for each local node.
Both the standalone service and the Asterisk controller use these settings.

## Set up an account

Add `[echolink]` for shared defaults. Add `[echolink 524950]` for one local
node. Replace `524950` with your controller node number. Its normal node
section must also exist. Each account needs its own `callsign` and EchoLink
`node_number`; those two fields never inherit from shared defaults.

```ini
[echolink]
enabled = no
announcement = node
telemetry = demand

[echolink 524950]
enabled = yes
callsign = K1ABC-R
node_number = 123456
name = Example repeater
location = Your town
email = operator@example.invalid
transport = direct
```

Use your own validated callsign and EchoLink number, not the example account.
Start with links disconnected and test local audio first.

## Store passwords

Keep passwords out of `rpt_advanced.conf`. The default file is
`echolink-secrets.conf` beside the controller configuration. Set `secrets_file`
to use another file. Relative paths start in that same configuration folder.

```ini
[524950]
directory_password = YOUR_ECHOLINK_PASSWORD
proxy_password = YOUR_PROXY_PASSWORD
```

The heading is the local controller node, not the EchoLink number. Omit
`proxy_password` when using direct mode. Proxy and directory passwords are
separate values.

The file must be a regular file, not a symbolic link. Its owner must be the
user running the controller, and its mode must be exactly `0600`.

| Controller | Usual file | Owner |
| --- | --- | --- |
| Standalone | `/etc/rpt_advanced/echolink-secrets.conf` | `rpt-advanced` |
| Asterisk | `/etc/asterisk/echolink-secrets.conf` | Your Asterisk service user |

Set ownership and mode after saving the file. For standalone:

```sh
sudo chown rpt-advanced:rpt-advanced /etc/rpt_advanced/echolink-secrets.conf
sudo chmod 0600 /etc/rpt_advanced/echolink-secrets.conf
```

Provision a separate file for each service user when switching controllers.
Do not share service privileges or loosen the file mode.
`--check-secrets-file` checks the AllStarLink secrets file, not this file.
EchoLink credentials are checked when the account is prepared at start or reload.

## Choose direct or Proxy service

Direct mode uses IPv4. Allow outbound directory TCP port `5200`, plus UDP
audio/text port `5198` and control port `5199`. Set the router and firewall
to pass the UDP ports to this host. `audio_port` changes the audio port;
the control port is always one higher.

For a user-provided EchoLink Proxy:

```ini
[echolink 524950]
transport = proxy
proxy_host = proxy.example.invalid
proxy_port = 8100
```

Set the Proxy password in the secrets file. A standard EchoLink Proxy carries
directory, audio, control, and text traffic over TCP. An HTTP proxy cannot do
this job. A failed Proxy connection does not switch to direct mode.

Each account needs a separate reachable public endpoint: its public IPv4 address
and UDP ports, often supplied by a separate Proxy. Different local UDP ports
alone do not make several accounts reachable through one public address.
Each standard Proxy serves one client. Do not
assign the same Proxy endpoint to two accounts on this host.

## Settings

Each setting below may be shared or changed per account unless noted.
An empty node value clears an inherited text or list value.

| Setting | Default | Use |
| --- | --- | --- |
| `enabled` | `no` | Enable this account. |
| `callsign` | empty | Account callsign; set per node. |
| `node_number` | empty | Complete positive EchoLink number; set per node. |
| `name`, `location`, `email`, `status_message` | empty | Public station details. |
| `latitude`, `longitude` | `0` | Decimal degrees north/east; use negative values for south/west. |
| `frequency_mhz`, `tone_hz`, `power_w` | `0` | Advertised RF frequency, tone, and power. These do not tune the radio. |
| `antenna_height_m`, `antenna_gain_db` | `0` | Advertised antenna height and gain. |
| `antenna_direction_deg` | `0` | Antenna direction: zero is north; `-1` means all directions. |
| `aprs_server` | `aprs.echolink.org:5199` | Server for public RF and location reports. Leave empty to turn reports off. |
| `directory_servers` | `servers.echolink.org,backup.echolink.org` | Ordered directory servers, separated by commas. |
| `max_stations` | `20` | Most connected stations on this account. |
| `max_pending_handshakes` | `20` | Most waiting incoming calls. |
| `heartbeat_interval_ms` | `10000` | Time between automatic connection checks. |
| `heartbeat_timeout_ms` | `100000` | Time without valid peer traffic before disconnect. |
| `connect_timeout_ms` | `30000` | Time allowed for a new call to connect. |
| `directory_timeout_ms` | `10000` | Time allowed for each directory operation. |
| `registration_refresh_ms` | `300000` | Time between account renewals. |
| `directory_refresh_ms` | `240000` | Time between station-list updates. |
| `directory_retry_ms` | `20000` | Wait after a failed directory operation before trying the next server. |
| `directory_freshness_ms` | `900000` | Oldest station list allowed for a new call. |
| `receive_gain_db` | `-3` | EchoLink receive gain, from -60 to +60 dB. |
| `transmit_gain_db` | `3` | EchoLink send gain, from -60 to +60 dB. |
| `announcement` | `node` | Connection notice: `off`, `node`, `callsign`, or `both`. Both says the node number first. |
| `telemetry` | `demand` | Peer telemetry: `off`, `on`, `follow`, or `demand`. |
| `telemetry_dynamic` | `yes` | Allow an unlocked administrator to change that mode by command. |
| `transport` | `direct` | `direct` or `proxy`. |
| `bind_address` | `0.0.0.0` | Local direct-mode IPv4 address. |
| `audio_port` | `5198` | Direct audio port, 1–65534. Control uses this port plus one. |
| `proxy_host` | empty | Standard EchoLink Proxy host. Required in Proxy mode. |
| `proxy_port` | `8100` | Proxy TCP port, 1–65535. |
| `allow`, `deny` | empty | Callsign patterns separated by commas; `*` and `?` are wildcards. Deny wins. |
| `secrets_file` | beside configuration | Private password file. |

All timing values are positive whole milliseconds; 1000 ms is one second.
Station limits are positive
whole numbers. No `allow` list means any directory-verified station may call.
An allow entry does not replace directory and address checks.

Public location coordinates round to hundredths of a minute. Frequency rounds
to kHz. The advertised tone uses whole Hz. Power, height, and antenna gain use
the nearest available PHG digit, from 0 through 9. The smallest advertised
height is 10 feet. These display limits do not change radio settings.

RF and location reports go to `aprs_server` ten seconds after the account
starts, then every ten minutes. Use `hostname:port` or `IPv4:port`. Direct mode
allows a custom UDP port. A standard EchoLink Proxy requires port `5199`.
Reports contain your configured public station details. Failed reports do not
end calls.

Telemetry means the controller's spoken status and connection notices.
`on` allows those notices when normal routing permits. `off` blocks them to
EchoLink stations. `follow` follows the local telemetry setting. `demand`
allows notices for two minutes after a command that calls for a reply.
These modes do not send IDs, courtesy tones, transmitter hang time, or spoken
chat to peers. A command reply still goes only to the station that requested it.

To assign a telemetry-mode DTMF prefix, set `link_command_echo_telemetry` in
the node or `[general]` section. It has no default prefix. Append `0`, `1`, `2`,
or `3` for off, on, follow, or demand. Use an unused prefix and the normal admin
unlock. `telemetry_dynamic = no` blocks live mode changes, not configuration reload.

## Make and end calls

Use the normal [link commands](allstarlink-status.md#control-links-by-radio).
Prefix the EchoLink number with `3`, padding the number to at least six digits.
End the command with `#` or unkey. Do not omit digits from a large number.

| EchoLink station | Dial alias | Default transceive command |
| --- | --- | --- |
| `123456` | `3123456` | `*33123456#` |
| `1002775` | `31002775` | `*331002775#` |

Use that same alias for monitor, disconnect, permanent links, and scheduled
groups. `*10` ends temporary calls; `*806` pauses permanent calls; `*816` resumes
them. Ordinary calls do not retry after disconnect. Configured permanent and
scheduled calls follow their usual retry rules. Callsign dialing and `*4`
remote-command forwarding are not available for EchoLink destinations.

Voice uses GSM at 8 kHz. Narrowband Speex is used only when both stations
support it. Among EchoLink stations on one account, the first active talker
holds the turn until its audio drains and its silence interval ends.

Incoming chat is spoken only on the local transmitter, with the sender named.
It mixes with link audio. Text can be passed as text to eligible peers, but
directed text is not widened into a broadcast. Station information and control
messages are not spoken. There is no outgoing chat entry or text-command control.

## Reload and check

Use the normal [configuration reload](install.md#apply-settings-and-upgrades).
Account metadata, policy, gain, and timing changes keep other accounts running.
Identity, password, and transport changes may reconnect the changed account.
A failed change restores its prior settings and attempts to reconnect; check
the log rather than assuming the old account is online.

A lost Proxy connection is retried in the background. After it reconnects,
the account renews its login and gets a fresh station list. Ordinary calls
stay disconnected; permanent and scheduled links follow their retry rules.

In Asterisk, use `rpt_advanced link status 524950`. Its account status shows
registration, directory age, transport, and failures. Peer status shows the
full station ID, codec, talker, audio queue, and first disconnect cause.

For standalone, inspect `journalctl -u rpt-advanced` for startup, retry, and
connection failures. Send `*70` by radio to hear direct-link status. A successful
directory login does not prove that an outside station can reach your UDP ports.

EchoLink does not publish AllStarLink topology. Avoid connecting paths that
feed back through the same network. Text relays cannot guarantee loop removal
across every network. Legacy UDP packets can also be indistinguishable from
packets from a newly reconnected station at the same address.

Validate both audio directions, silence, disconnect, text, and reload with the
clients and Proxy you intend to use. See the [acceptance checks](testing.md#echolink-checks).
