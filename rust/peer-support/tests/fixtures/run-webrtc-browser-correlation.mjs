// Run with node run-webrtc-browser-correlation.mjs /path/to/chromium-or-edge
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";

const executable = process.argv[2];
if (!executable) throw new Error("Supply the path to an installed Chromium browser");
const page = await readFile(new URL("./webrtc-browser-correlation.html", import.meta.url));
const profile = await mkdtemp(join(tmpdir(), "rptadv-browser-fixture-"));
let result;
const reply = new Promise(resolve => { result = resolve; });
const server = createServer(async (request, response) => {
  if (request.method === "GET" && request.url === "/") {
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" }).end(page);
  } else if (request.method === "POST" && request.url === "/result") {
    const chunks = [];
    let length = 0;
    for await (const chunk of request) {
      length += chunk.length;
      if (length > 4096) {
        response.writeHead(413).end();
        return;
      }
      chunks.push(chunk);
    }
    response.writeHead(204).end();
    result(Buffer.concat(chunks).toString("utf8"));
  } else {
    response.writeHead(404).end();
  }
});

let browser;
let deadline;
let exited;
try {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  browser = spawn(executable, [
    "--headless=new", "--no-first-run", "--no-default-browser-check",
    "--disable-background-networking", "--disable-background-timer-throttling",
    "--disable-renderer-backgrounding", "--autoplay-policy=no-user-gesture-required",
    "--mute-audio", "--user-data-dir=" + profile,
    // Only the fixture's synthetic sender needs timestamp injection. The receiver is standard.
    "--enable-blink-features=RTCEncodedFrameSetMetadata",
    "http://127.0.0.1:" + server.address().port + "/",
  ], { stdio: "ignore", windowsHide: true });
  exited = new Promise(resolve => browser.once("exit", (code, signal) => {
    resolve({ code, signal });
  }));
  const failure = new Promise((_, reject) => {
    browser.once("error", reject);
    exited.then(({ code, signal }) => reject(new Error("Fixture browser exited: " + (signal || code))));
    deadline = setTimeout(() => reject(new Error("No browser fixture result before deadline")), 25_000);
  });
  const evidence = await Promise.race([reply, failure]);
  console.log(evidence);
  if (!JSON.parse(evidence).passed) process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  server.close();
  server.closeAllConnections();
  if (browser?.pid && browser.exitCode === null) {
    browser.kill();
    await exited;
  }
  // The only recursive removal is the explicitly created profile directory.
  await rm(profile, { recursive: true, force: true, maxRetries: 8, retryDelay: 125 });
}
