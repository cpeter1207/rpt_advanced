no-links =
    .text = NO LINKS
    .tts = No links
    .morse = NO LINKS

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
