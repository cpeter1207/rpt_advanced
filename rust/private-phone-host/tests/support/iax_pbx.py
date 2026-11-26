"""Run a private Asterisk registrar and, when requested, an inbound test call."""
import pathlib
import socket
import subprocess
import sys
import tempfile
import time


# Steps execute inside one authenticated native IAX channel on the private PBX.
# The manager observes each completed application before inspecting product audio.
DIGITS = {
    "wrong": "741#0000#", "login": "741#1234#", "attach": "*3#",
    "mute": "*99#", "unmute": "*99#",
}
MEDIA = ("beforeauth", "beforeattach", "joined", "muted", "restored")


def managed_dialplan(root):
    sound = root / "phone-test-tone"
    sound.with_suffix(".ulaw").write_bytes((bytes([0xa2]) * 8 + bytes([0x22]) * 8) * 320)
    steps = {name: f"SendDTMF({digits},90,100)" for name, digits in DIGITS.items()}
    steps.update({name: f"Playback({sound},noanswer)" for name in MEDIA})
    lines = ["[managed]", "exten => park,1,Wait(60)", "same => n,Hangup()"]
    for name, application in steps.items():
        lines.extend([
            f"exten => {name},1,{application}",
            f"same => n,Set(PHONE_FIXTURE_STEP={name})",
            "same => n,Goto(park,1)",
        ])
    return "\n".join(lines) + "\n"


def inbound_commands(command):
    def cli(text):
        output = subprocess.run(command + ["-rx", text], check=True, capture_output=True,
                                text=True, timeout=4)
        return output.stdout

    def until(read):
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            value = read()
            if value:
                return value
            time.sleep(0.04)
        raise RuntimeError("Asterisk did not complete the requested call step")

    channel = None
    for line in sys.stdin:
        step = line.strip()
        if step == "dial" and channel is None:
            cli("channel originate IAX2/club-account/incoming-extension extension park@managed")

            def connected():
                for row in cli("core show channels concise").splitlines():
                    fields = row.split("!")
                    if (len(fields) > 5 and fields[0].startswith("IAX2/club-account-")
                            and fields[1:3] == ["managed", "park"] and fields[4] == "Up"
                            and fields[5] == "Wait"):
                        return fields[0]
                return None

            channel = until(connected)
        elif step in DIGITS or step in MEDIA:
            if not channel:
                raise ValueError("a connected call is required")
            cli(f"channel redirect {channel} managed,{step},1")
            until(lambda: any(row.strip() == f"PHONE_FIXTURE_STEP={step}"
                              for row in cli(f"core show channel {channel}").splitlines()))
        elif step == "channel-ended" and channel:
            until(lambda: not any(row.startswith(channel + "!") for row in
                                  cli("core show channels concise").splitlines()))
        else:
            raise ValueError(f"unexpected call step {step!r}")
        if step in MEDIA:
            state = cli(f"core show channel {channel}")
            fields = [row.strip() for row in state.splitlines() if row.strip().startswith(
                ("PLAYBACKSTATUS=", "NativeFormats:", "ReadFormat:", "WriteFormat:"))]
            # Observe Asterisk's own channel counters without exposing channel variables or digits.
            frames = {}
            for row in state.splitlines():
                name, _, value = row.strip().partition(":")
                if name in ("Frames in", "Frames out") and value.strip().split():
                    first = value.strip().split()[0]
                    if first.isdecimal():
                        frames[name] = int(first)
            print(f"managed IAX {step}: {fields}, channel frames {frames}", file=sys.stderr, flush=True)
            if "PLAYBACKSTATUS=SUCCESS" not in fields:
                raise RuntimeError(f"Asterisk failed the actual {step} media application")
        print(step, channel, flush=True)


def concurrent_outgoing_commands(command):
    """Report the exact unanswered extension and observe actual channel retirement."""
    def cli(text):
        return subprocess.run(command + ["-rx", text], check=True, capture_output=True,
                              text=True, timeout=2).stdout

    def channels():
        return [fields for row in cli("core show channels concise").splitlines()
                if len(fields := row.split("!")) > 5 and fields[0].startswith("IAX2/")]

    held = None
    for line in sys.stdin:
        step = line.strip()
        if step == "held":
            waiting = [row[0] for row in channels()
                       if row[1:3] == ["phone-test", "998"] and row[4] != "Up" and row[5] == "Wait"]
            if len(waiting) > 1 or (waiting and held not in (None, waiting[0])):
                raise RuntimeError("unexpected held IAX leg")
            if waiting:
                held = waiting[0]
            print(waiting[0] if waiting else "-", flush=True)
            continue
        if step not in ("wait-held-ended", "cleanup-held", "wait-all-ended") or not held:
            raise ValueError("unexpected concurrent IAX fixture command")
        if step == "cleanup-held" and any(row[0] == held for row in channels()):
            cli(f"channel request hangup {held}")
        deadline = time.monotonic() + 2
        while True:
            live = [row[0] for row in channels()]
            pending = bool(live) if step == "wait-all-ended" else held in live
            if not pending or time.monotonic() >= deadline:
                print("pending" if pending else "ended", flush=True)
                break
            time.sleep(0.04)


def run():
    managed = sys.argv[1:] == ["--managed-call"]
    concurrent = sys.argv[1:] == ["--concurrent-outgoing"]
    if sys.argv[1:] and not (managed or concurrent):
        raise ValueError("unknown private PBX fixture mode")
    with tempfile.TemporaryDirectory(prefix="phone-account-") as temporary:
        root = pathlib.Path(temporary)
        module = next(pathlib.Path("/usr/lib").glob("*/asterisk/modules/chan_iax2.so"))
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        config = root / "asterisk.conf"
        directories = "astetcdir astvarlibdir astdbdir astkeydir astagidir astspooldir astrundir astlogdir astcachedir".split()
        config.write_text(
            "[directories]\n" + "".join(f"{key} => {root}\n" for key in directories)
            + f"astmoddir => {module.parent}\nastdatadir => /usr/share/asterisk\n"
            + "[options]\nverbose=0\ndebug=0\n"
        )
        (root / "modules.conf").write_text(
            "[modules]\nautoload=no\nload=chan_iax2.so\nload=pbx_config.so\n"
            "load=codec_ulaw.so\nload=app_echo.so\n"
            + ("load=res_clioriginate.so\nload=app_senddtmf.so\nload=app_playback.so\n"
               "load=format_pcm.so\n" if managed else "")
        )
        (root / "logger.conf").write_text("[logfiles]\nconsole => warning,error\n")
        (root / "extensions.conf").write_text(
            "[phone-test]\nexten => 999,1,Answer()\nsame => n,Echo()\nsame => n,Hangup()\n"
            + ("exten => 998,1,Wait(60)\nsame => n,Hangup()\n" if concurrent else "")
            + (managed_dialplan(root) if managed else "")
        )
        (root / "iax.conf").write_text(
            f"[general]\nbindport={port}\nbindaddr=127.0.0.1\ndisallow=all\nallow=ulaw\n"
            "[club-account]\ntype=friend\nhost=dynamic\nusername=club-account\nrequirecalltoken=yes\n"
            "auth=md5\nsecret=fixture-only\ncontext=phone-test\ndeny=0.0.0.0/0.0.0.0\n"
            "permit=127.0.0.1/255.255.255.255\n"
        )
        command = ["asterisk", "-C", str(config)]
        with subprocess.Popen(command + ["-f", "-g"], stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL) as pbx:
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and pbx.poll() is None:
                    status = subprocess.run(command + ["-rx", "iax2 show users"],
                                            capture_output=True, text=True, timeout=2)
                    if status.returncode == 0 and "club-account" in status.stdout:
                        break
                    time.sleep(0.04)
                else:
                    raise RuntimeError("private IAX registrar failed to start")
                print(port, flush=True)
                if managed:
                    inbound_commands(command)
                elif concurrent:
                    concurrent_outgoing_commands(command)
                else:
                    sys.stdin.buffer.read()
            finally:
                if pbx.poll() is None:
                    pbx.terminate()
                    try:
                        pbx.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        pbx.kill()
                        pbx.wait()


if __name__ == "__main__":
    run()
