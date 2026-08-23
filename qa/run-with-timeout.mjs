#!/usr/bin/env node
import { spawn } from "node:child_process";

const [timeoutText, command, ...args] = process.argv.slice(2);
const timeoutMs = Number.parseInt(timeoutText ?? "", 10);

if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || !command) {
  console.error("usage: run-with-timeout.mjs <milliseconds> <command> [args...]");
  process.exit(2);
}

const child = spawn(command, args, { stdio: "inherit", windowsHide: true });
let timedOut = false;
const timer = setTimeout(() => {
  timedOut = true;
  child.kill("SIGTERM");
  setTimeout(() => child.kill("SIGKILL"), 1_000).unref();
}, timeoutMs);

child.once("error", (error) => {
  clearTimeout(timer);
  console.error(`failed to start ${command}: ${error.message}`);
  process.exitCode = 127;
});

child.once("exit", (code, signal) => {
  clearTimeout(timer);
  if (timedOut) {
    console.error(`${command} exceeded ${timeoutMs} ms and was terminated`);
    process.exitCode = 124;
  } else if (signal) {
    console.error(`${command} terminated by ${signal}`);
    process.exitCode = 1;
  } else {
    process.exitCode = code ?? 1;
  }
});
