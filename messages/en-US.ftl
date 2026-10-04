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

loop-rejected =
    .text = LINK TO NODE { $node } REJECTED: WOULD CREATE LOOP
    .tts = Connection to { $node } rejected because it would create a link loop
    .morse = LINK TO NODE { $node } REJECTED WOULD CREATE LOOP

priority-group-selected =
    .text = { $group }: NODE { $peer } SELECTED
    .tts = { $group }. { $peer } selected
    .morse = { $group } NODE { $peer } SELECTED

priority-group-unavailable =
    .text = { $group } UNAVAILABLE
    .tts = { $group } is unavailable
    .morse = { $group } UNAVAILABLE

scheduled-link-change =
    .text = CONNECTION CHANGES IN { $time_remaining }
    .tts = This connection will change in { $time_remaining }
    .morse = CONNECTION CHANGES IN { $time_remaining }

duration-seconds =
    .text = { $seconds } seconds
    .tts = { $seconds } seconds
    .morse = { $seconds } SECONDS

parrot-levels =
    .text = Peak level { $peak_dbfs } dBFS. RMS level { $rms_dbfs } dBFS.
    .tts = Peak level { $peak_dbfs } dBFS. RMS level { $rms_dbfs } dBFS.
    .morse = PEAK { $peak_dbfs } DBFS RMS { $rms_dbfs } DBFS
