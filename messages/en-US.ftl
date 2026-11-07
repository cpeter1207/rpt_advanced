no-links =
    .text = NO LINKS
    .tts = No links
    .morse = NO LINKS

incoming-text =
    .text = { $source } says: { $body }
    .tts = { $kind ->
        [node] Node { $source } says: { $body }
       *[callsign] { $source } says: { $body }
        }
    .morse = { $source } SAYS { $body }

peer-connected-local =
    .text = { $peer } connected
    .tts = { $peer } connected
    .morse = { $peer } CONNECTED

peer-disconnected-local =
    .text = { $peer } disconnected
    .tts = { $peer } disconnected
    .morse = { $peer } DISCONNECTED

link-status =
    .text = { $count ->
        [1] LINK { $peer } { $mode ->
            [transceive] TRANSCEIVE
            [monitor] MONITOR
           *[local] LOCAL
        }
       *[other] { $count } LINKS { $peer } { $mode ->
            [transceive] TRANSCEIVE
            [monitor] MONITOR
           *[local] LOCAL
        }
    }
    .tts = { $count ->
        [1] Link { $peer } { $mode ->
            [transceive] transceive
            [monitor] monitor
           *[local] local
        }
       *[other] { $count } links. { $peer } { $mode ->
            [transceive] transceive
            [monitor] monitor
           *[local] local
        }
    }
    .morse = { $count ->
        [1] LINK { $peer } { $mode ->
            [transceive] TRANSCEIVE
            [monitor] MONITOR
           *[local] LOCAL
        }
       *[other] { $count } LINKS { $peer } { $mode ->
            [transceive] TRANSCEIVE
            [monitor] MONITOR
           *[local] LOCAL
        }
    }

last-keyed-none =
    .text = NO LAST KEYED
    .tts = No last keyed station
    .morse = NO LAST KEYED

last-keyed =
    .text = LAST KEYED { $peer }
    .tts = Last keyed node { $peer }
    .morse = LAST KEYED { $peer }

peer-connected-third-party =
    .text = { $first } CONNECTED TO { $second }
    .tts = { $first } connected to { $second }
    .morse = { $first } CONNECTED TO { $second }

peer-disconnected-third-party =
    .text = { $first } DISCONNECTED FROM { $second }
    .tts = { $first } disconnected from { $second }
    .morse = { $first } DISCONNECTED FROM { $second }

day-of-week =
    .text = { $day ->
        [sunday] Sunday
        [monday] Monday
        [tuesday] Tuesday
        [wednesday] Wednesday
        [thursday] Thursday
        [friday] Friday
        *[saturday] Saturday
    }
    .tts = { $day ->
        [sunday] Sunday
        [monday] Monday
        [tuesday] Tuesday
        [wednesday] Wednesday
        [thursday] Thursday
        [friday] Friday
        *[saturday] Saturday
    }
    .morse = { $day ->
        [sunday] SUNDAY
        [monday] MONDAY
        [tuesday] TUESDAY
        [wednesday] WEDNESDAY
        [thursday] THURSDAY
        [friday] FRIDAY
        *[saturday] SATURDAY
    }

greeting-morning =
    .text = Good Morning
    .tts = Good Morning
    .morse = GOOD MORNING

greeting-afternoon =
    .text = Good Afternoon
    .tts = Good Afternoon
    .morse = GOOD AFTERNOON

greeting-evening =
    .text = Good Evening
    .tts = Good Evening
    .morse = GOOD EVENING

time-announcement =
    .text = { $greeting_text }. The time is { $time }.
    .tts = { $greeting_tts }. The time is { $time }.
    .morse = { $time }

binding-node =
    .text = { $node }
    .tts = node,{ $digits }
    .morse = { $node }

binding-date =
    .text = { $date }
    .tts = { $date }
    .morse = { $date }

binding-time =
    .text = { $clock }
    .tts = { $clock }
    .morse = { $clock }

site-time =
    .text = { $time }
    .tts = The time is { $time }.
    .morse = { $time }

site-temperature =
    .text = Temperature: { $temperature }
    .tts = Temperature is { $temperature }.
    .morse = TEMP { $temperature }

site-link-status =
    .text = { $links }
    .tts = { $links }
    .morse = { $links }

site-value-unavailable =
    .text = { $label } unavailable
    .tts = { $label } is unavailable.
    .morse = { $label } UNAVAILABLE

site-query-unavailable =
    .text = Requested site reading unavailable
    .tts = The requested site reading is unavailable.
    .morse = SITE READING UNAVAILABLE

site-meter-report =
    .text = { $label }: { $value }{ $unit }
    .tts = { $label } is { $value }{ $unit }.
    .morse = { $label } { $value }{ $unit }

site-input-active =
    .text = { $label } active
    .tts = { $label } is active.
    .morse = { $label } ACTIVE

site-input-inactive =
    .text = { $label } inactive
    .tts = { $label } is inactive.
    .morse = { $label } INACTIVE

site-unit-volts =
    .text = volts
    .tts = volts
    .morse = V

site-unit-amps =
    .text = amps
    .tts = amps
    .morse = A

site-unit-celsius =
    .text = degrees Celsius
    .tts = degrees Celsius
    .morse = C

site-unit-watts =
    .text = watts
    .tts = watts
    .morse = W

site-unit-joules =
    .text = joules
    .tts = joules
    .morse = J

site-unit-rpm =
    .text = RPM
    .tts = revolutions per minute
    .morse = RPM

site-unit-percent =
    .text = percent
    .tts = percent
    .morse = PERCENT

loop-rejected =
    .text = LINK TO NODE { $node } REJECTED: WOULD CREATE LOOP
    .tts = Connection to { $node } rejected because it would create a link loop
    .morse = LINK TO NODE { $node } REJECTED WOULD CREATE LOOP

remote-command-unsupported =
    .text = REMOTE COMMANDS NOT SUPPORTED FOR NODE { $node }
    .tts = Remote commands are not supported for { $node }
    .morse = REMOTE COMMANDS NOT SUPPORTED FOR NODE { $node }

echo-peer-identity =
    .text = { $mode ->
        [callsign] { $callsign }
        [both] node { $node } { $callsign }
       *[node] node { $node }
    }
    .tts = { $mode ->
        [callsign] { $callsign }
        [both] node { $node } { $callsign }
       *[node] node { $node }
    }
    .morse = { $mode ->
        [callsign] { $callsign }
        [both] NODE { $node } { $callsign }
       *[node] NODE { $node }
    }

priority-group-selected =
    .text = { $group }: NODE { $peer } CONNECTED
    .tts = { $group }. { $peer } connected
    .morse = { $group } NODE { $peer } CONNECTED

priority-group-unavailable =
    .text = { $group } UNAVAILABLE
    .tts = { $group } is unavailable
    .morse = { $group } UNAVAILABLE

scheduled-link-change =
    .text = { $direction ->
        [connect] Connecting to { $target } in { $time_remaining }
       *[disconnect] Disconnecting from { $target } in { $time_remaining }
        }
    .tts = { scheduled-link-change.text }
    .morse = { $direction ->
        [connect] CONNECTING TO { $target } IN { $time_remaining }
       *[disconnect] DISCONNECTING FROM { $target } IN { $time_remaining }
        }

repeater-timed-out =
    .text = Repeater timed out after { $duration }.
    .tts = Repeater timed out after { $duration }.
    .morse = REPEATER TIMED OUT AFTER { $duration }

repeater-timeout-cleared =
    .text = Repeater timeout cleared.
    .tts = Repeater timeout cleared.
    .morse = REPEATER TIMEOUT CLEARED

duration-unit =
    .text = { $unit ->
        [month] { $count ->
            [one] { $count } month
           *[other] { $count } months
            }
        [week] { $count ->
            [one] { $count } week
           *[other] { $count } weeks
            }
        [day] { $count ->
            [one] { $count } day
           *[other] { $count } days
            }
        [hour] { $count ->
            [one] { $count } hour
           *[other] { $count } hours
            }
        [minute] { $count ->
            [one] { $count } minute
           *[other] { $count } minutes
            }
       *[second] { $count ->
            [one] { $count } second
           *[other] { $count } seconds
            }
        }
    .tts = { duration-unit.text }
    .morse = { $count }{ $unit ->
        [month] M
        [week] W
        [day] D
        [hour] H
        [minute] M
       *[second] S
        }

duration-join =
    .text = { $position ->
        [last] { $head }, and { $tail }
       *[more] { $head }, { $tail }
        }
    .tts = { duration-join.text }
    .morse = { $head } { $tail }

parrot-levels =
    .text = Peak level { $peak_dbfs } dBFS. RMS level { $rms_dbfs } dBFS.
    .tts = Peak level { $peak_dbfs } dBFS. RMS level { $rms_dbfs } dBFS.
    .morse = PEAK { $peak_dbfs } DBFS RMS { $rms_dbfs } DBFS
pad-test-empty =
    .text = no digits
    .tts = no digits
    .morse = NO DIGITS
pad-test-overflow =
    .text = too many digits
    .tts = too many digits
    .morse = TOO MANY DIGITS
dtmf-symbol =
    .text = { $symbol }
    .tts = { $kind ->
        [star] star
        [pound] pound
       *[other] { $symbol }
        }
    .morse = { $kind ->
        [star] STAR
        [pound] POUND
       *[other] { $symbol }
        }

# Remote-base results. Selector numbers are stable machine identifiers.
remote-applied =
    .text = { remote-applied.tts }
    .tts = Remote radio command completed.
    .morse = REMOTE OK
remote-error =
    .text = { remote-error.tts }
    .tts = { $reason ->
        [busy] Remote radio is busy. Try again.
        [stopped] Remote radio is unavailable.
        [stale_source] Remote radio command canceled.
       *[rejected] Remote radio command failed. Error { $code }.
        }
    .morse = REMOTE { $reason ->
        [busy] BUSY
        [stopped] STOP
        [stale_source] CANCEL
       *[rejected] ERR { $code }
        }
remote-unit =
    .text = { remote-unit.tts }
    .tts = { $units ->
        [1] hertz
        [2] milliwatts
        [3] D C S
        [4] decibels
        [5] words per minute
        [6] ratio
       *[0] { "" }
        }
    .morse = { $units ->
        [1] HZ
        [2] MW
        [3] DCS
        [4] DB
        [5] WPM
        [6] RATIO
       *[0] { "" }
        }
remote-setting-name =
    .text = { remote-setting-name.tts }
    .tts = { $selector ->
        [1] Receive frequency
        [2] Transmit frequency
        [3] Mode
        [4] Passband width
        [5] Receive V F O
        [6] Transmit V F O
        [7] Split
        [8] Repeater offset
        [9] Receive incremental tuning
        [10] Transmit incremental tuning
        [11] Transmit power
        [12] Transmit C T C S S
        [13] Receive C T C S S
        [14] Transmit D C S
        [15] Receive D C S
        [16] C T C S S encode
        [17] C T C S S decode
        [20] Push to talk
        [21] Carrier detect
        [100] Audio gain
        [101] Radio frequency gain
        [102] Microphone gain
        [103] Squelch
        [104] Compression
        [105] Automatic gain control
        [106] I F shift
        [107] Preamplifier
        [108] Attenuator
        [109] Noise reduction
        [110] Notch
        [111] Keyer speed
        [200] Antenna
        [300] Noise blanker
        [301] Noise reduction enabled
        [302] Automatic notch
        [303] Manual notch
        [304] Compressor enabled
        [305] Antenna tuner enabled
        [306] Monitor
        [307] Receive incremental tuning enabled
        [308] Transmit incremental tuning enabled
        [400] Encode source
        [401] Decode signal source
        [402] Decode programming source
        [403] Transmit D C S inverted
        [404] Receive D C S inverted
       *[other] Setting { $selector }
        }
    .morse = SET { $selector }
remote-value =
    .text = { remote-value.tts }
    .tts = { remote-setting-name.tts } is { $value } { remote-unit.tts }.
    .morse = { remote-setting-name.morse } { $value } { remote-unit.morse }
remote-state =
    .text = { remote-state.tts }
    .tts = { $state ->
        [requested] Requested settings.
        [applied] Applied settings.
        [observed] Observed settings.
       *[memory] Memory settings.
        } { $available ->
        [0] Unavailable.
       *[1] { "" }
        }
    .morse = { $state ->
        [requested] REQ
        [applied] APPL
        [observed] OBS
       *[memory] MEM
        } { $available ->
        [0] N A
       *[1] { "" }
        }
remote-field-name =
    .text = { remote-field-name.tts }
    .tts = { $field ->
        [rx_hz] Receive hertz
        [tx_hz] Transmit hertz
        [mode] Mode
        [bandwidth_hz] Passband hertz
        [rx_vfo] Receive V F O
        [tx_vfo] Transmit V F O
        [split] Split enabled
        [offset_hz] Offset hertz
        [rit_hz] Receive incremental tuning hertz
        [rit_enabled] Receive incremental tuning enabled
        [xit_hz] Transmit incremental tuning hertz
        [xit_enabled] Transmit incremental tuning enabled
        [power_mw] Power milliwatts
        [channel] Channel
        [encode_source] Encoder source
        [encode_control_source] Encoder control source
        [encode_enabled] Encoder enabled
        [encode_ctcss_tenths_hz] Encode C T C S S hertz
        [encode_dcs_bits] Encode D C S code
        [encode_inverted] Encode inverted
        [decode_source] Decoder source
        [decode_control_source] Decoder control source
        [decode_enabled] Decoder enabled
        [decode_ctcss_tenths_hz] Decode C T C S S hertz
        [decode_dcs_bits] Decode D C S code
        [decode_inverted] Decode inverted
        [tx_inhibited] Transmit inhibited
        [receive_permitted] Receive permitted
        [serviced_ptt] Serviced push to talk
        [running] Scan running
        [paused] Scan paused
        [list] Scan list
        [activity_hold] Scan activity hold
        [direction] Scan direction
        [priority] Priority memory
        [failed_memory] Failed memory
        [last_error] Last scan error
        [deadline_ms] Next action time in milliseconds
        [skipped] Skipped members
        [dwell_ms] Dwell milliseconds
        [resume_delay_ms] Resume delay milliseconds
       *[memory] Current memory
        }
    .morse = { $field ->
        [rx_hz] RX HZ
        [tx_hz] TX HZ
        [mode] MODE
        [bandwidth_hz] BW HZ
        [rx_vfo] RX VFO
        [tx_vfo] TX VFO
        [split] SPLIT
        [offset_hz] OFFSET HZ
        [rit_hz] RIT HZ
        [rit_enabled] RIT
        [xit_hz] XIT HZ
        [xit_enabled] XIT
        [power_mw] PWR MW
        [channel] CH
        [encode_source] ENC SRC
        [encode_control_source] ENC CTRL
        [encode_enabled] ENC
        [encode_ctcss_tenths_hz] ENC HZ
        [encode_dcs_bits] ENC DCS
        [encode_inverted] ENC INV
        [decode_source] DEC SRC
        [decode_control_source] DEC CTRL
        [decode_enabled] DEC
        [decode_ctcss_tenths_hz] DEC HZ
        [decode_dcs_bits] DEC DCS
        [decode_inverted] DEC INV
        [tx_inhibited] TX INH
        [receive_permitted] RX OK
        [serviced_ptt] PTT SET
        [running] SCAN
        [paused] PAUSE
        [list] LIST
        [activity_hold] HOLD
        [direction] DIR
        [priority] PRI
        [failed_memory] FAIL MEM
        [last_error] SCAN ERR
        [deadline_ms] NEXT MS
        [skipped] SKIP
        [dwell_ms] DWELL MS
        [resume_delay_ms] RESUME MS
       *[memory] MEM
        }
remote-field-value =
    .text = { remote-field-value.tts }
    .tts = { $field ->
        [mode] { $enum ->
            [1] F M
            [2] A M
            [3] Upper sideband
            [4] Lower sideband
           *[other] { $value }
            }
        [encode_source] { $enum ->
            [1] Controller D S P
            [2] External
            [3] Radio
           *[other] { $value }
            }
        [decode_source] { $enum ->
            [1] Controller D S P
            [2] External
            [3] Radio
           *[other] { $value }
            }
       *[other] { $value }
        }
    .morse = { $field ->
        [mode] { $enum ->
            [1] FM
            [2] AM
            [3] USB
            [4] LSB
           *[other] { $value }
            }
       *[other] { $value }
        }
remote-field =
    .text = { remote-field.tts }
    .tts = { remote-field-name.tts } { $available ->
        [0] unavailable
       *[1] { remote-field-value.tts }
        }.
    .morse = { remote-field-name.morse } { $available ->
        [0] N A
       *[1] { remote-field-value.morse }
        }
remote-entry =
    .text = { remote-entry.tts }
    .tts = { $catalog ->
        [memories] Memory
       *[scans] Scan list
        } { $selector }, { $name }.
    .morse = { $catalog ->
        [memories] MEM
       *[scans] SCAN
        } { $selector } { $name }
remote-capability =
    .text = { remote-capability.tts }
    .tts = { remote-setting-name.tts }, selector { $selector }. Read { $read }, write { $write }. { $minimum } to { $maximum } { remote-unit.tts }, { $places } decimal places.
    .morse = SET { $selector } R { $read } W { $write } MIN { $minimum } MAX { $maximum } { remote-unit.morse } DP { $places }
remote-choice =
    .text = { remote-choice.tts }
    .tts = Selector { $selector }, choice { $value }.
    .morse = SET { $selector } VAL { $value }
remote-empty =
    .text = { remote-empty.tts }
    .tts = No more { $catalog ->
        [capabilities] settings
        [memories] memories
       *[scans] scan lists
        }.
    .morse = END { $catalog ->
        [capabilities] SET
        [memories] MEM
       *[scans] SCAN
        }
