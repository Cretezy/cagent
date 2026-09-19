#!/usr/bin/env bun

const DEFAULT_RUNS = 3;

type JsonObject = Record<string, unknown>;

export type BenchmarkRun = {
  elapsedMs: number | null;
  sessionId: string | null;
  exitCode: number;
  usage: JsonObject | null;
  error: string | null;
  output: string | null;
};

export type BenchmarkOptions = {
  runs: number;
  specifications: string[];
};

export function parseBenchmarkOptions(arguments_: string[]): BenchmarkOptions {
  let runs = DEFAULT_RUNS;
  const specifications: string[] = [];

  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "-n" || argument === "--runs") {
      const value = arguments_[index + 1];
      if (value === undefined) throw new Error(`${argument} requires a positive integer`);
      if (!/^\d+$/.test(value) || Number(value) < 1 || !Number.isSafeInteger(Number(value))) {
        throw new Error(`${argument} must be a positive integer`);
      }
      runs = Number(value);
      index += 1;
    } else if (argument.startsWith("--runs=")) {
      const value = argument.slice("--runs=".length);
      if (!/^\d+$/.test(value) || Number(value) < 1 || !Number.isSafeInteger(Number(value))) {
        throw new Error("--runs must be a positive integer");
      }
      runs = Number(value);
    } else {
      specifications.push(argument);
    }
  }
  return { runs, specifications };
}

/** Parses one shell-style command argument without invoking a shell. */
export function parseCommand(input: string): string[] {
  const arguments_: string[] = [];
  let current = "";
  let quote: "'" | '"' | null = null;
  let escaped = false;
  let started = false;

  for (const character of input) {
    if (escaped) {
      current += character;
      escaped = false;
      started = true;
      continue;
    }
    if (character === "\\" && quote !== "'") {
      escaped = true;
      started = true;
      continue;
    }
    if (quote) {
      if (character === quote) quote = null;
      else current += character;
      started = true;
      continue;
    }
    if (character === "'" || character === '"') {
      quote = character;
      started = true;
    } else if (/\s/.test(character)) {
      if (started) {
        arguments_.push(current);
        current = "";
        started = false;
      }
    } else {
      current += character;
      started = true;
    }
  }

  if (escaped) throw new Error("command ends with an unfinished escape");
  if (quote) throw new Error(`command has an unclosed ${quote} quote`);
  if (started) arguments_.push(current);
  return arguments_;
}

function hasJsonOutput(arguments_: string[]): boolean {
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "--output") return arguments_[index + 1] === "json";
    if (argument.startsWith("--output=")) return argument.slice("--output=".length) === "json";
  }
  return false;
}

function hasOutputOption(arguments_: string[]): boolean {
  return arguments_.some((argument) => argument === "--output" || argument.startsWith("--output="));
}

function hasArchiveOption(arguments_: string[]): boolean {
  return arguments_.includes("--archive");
}

function hasNoSessionOption(arguments_: string[]): boolean {
  return arguments_.includes("--no-session");
}

export function commandForBenchmark(specification: string): string[] {
  const arguments_ = parseCommand(specification);
  if (arguments_.length === 0) throw new Error("benchmark command cannot be empty");
  if (hasOutputOption(arguments_) && !hasJsonOutput(arguments_)) {
    throw new Error("benchmark commands must use --output json");
  }
  if (!hasOutputOption(arguments_)) arguments_.push("--output", "json");
  if (!hasArchiveOption(arguments_) && !hasNoSessionOption(arguments_)) {
    arguments_.push("--archive");
  }
  const binary = `target/debug/cagent${process.platform === "win32" ? ".exe" : ""}`;
  return [binary, "exec", ...arguments_];
}

export function completedRecord(stdout: string): JsonObject | null {
  return eventRecord(stdout, "completed");
}

function eventRecord(stdout: string, event: string): JsonObject | null {
  let recordForEvent: JsonObject | null = null;
  for (const line of stdout.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const record: unknown = JSON.parse(line);
      if (
        record !== null
        && typeof record === "object"
        && (record as JsonObject).event === event
      ) {
        recordForEvent = record as JsonObject;
      }
    } catch {
      // A schema-constrained JSON result may be appended after the JSONL events.
    }
  }
  return recordForEvent;
}

function object(value: unknown): JsonObject | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as JsonObject
    : null;
}

async function runBenchmark(command: string[]): Promise<BenchmarkRun> {
  const childProcess = Bun.spawn(command, { cwd: process.cwd(), stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(childProcess.stdout).text(),
    new Response(childProcess.stderr).text(),
    childProcess.exited,
  ]);
  const record = completedRecord(stdout);
  const started = eventRecord(stdout, "started");
  const usage = object(record?.usage) ?? object(object(record?.stats)?.usage);
  const elapsedMs = numberAt(record, "elapsed_ms");
  const sessionId = stringAt(record, "session_id") ?? stringAt(started, "session_id");
  const error = exitCode === 0
    ? null
    : (stderr.trim() || `command exited with status ${exitCode}`).slice(0, 500);
  const output = exitCode === 0
    ? null
    : [stdout.trim(), stderr.trim()].filter(Boolean).join("\n").slice(0, 20_000);
  return { elapsedMs, sessionId, exitCode, usage, error, output };
}

async function buildCagent(): Promise<void> {
  const childProcess = Bun.spawn(["cargo", "build", "--quiet", "-p", "cagent-cli"], {
    cwd: process.cwd(),
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(childProcess.stdout).text(),
    new Response(childProcess.stderr).text(),
    childProcess.exited,
  ]);
  if (exitCode !== 0) throw new Error([stdout.trim(), stderr.trim()].filter(Boolean).join("\n") || "Cagent build failed");
}

function numberAt(value: JsonObject | null, key: string): number | null {
  const candidate = value?.[key];
  return typeof candidate === "number" && Number.isFinite(candidate) ? candidate : null;
}

function stringAt(value: JsonObject | null, key: string): string | null {
  const candidate = value?.[key];
  return typeof candidate === "string" ? candidate : null;
}

function averageNumber(values: Array<number | null>): string {
  const known = values.filter((value): value is number => value !== null);
  if (known.length !== values.length || known.length === 0) return "—";
  const average = known.reduce((sum, value) => sum + value, 0) / known.length;
  return Number.isInteger(average) ? String(average) : average.toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
}

export function averageInteger(values: Array<number | null>): string {
  const known = values.filter((value): value is number => value !== null);
  if (known.length !== values.length || known.length === 0) return "—";
  return String(Math.round(known.reduce((sum, value) => sum + value, 0) / known.length));
}

/** Averages decimal strings without converting monetary amounts to binary floats. */
export function averageDecimal(values: Array<string | null>): string {
  if (values.length === 0 || values.some((value) => value === null)) return "—";
  const parts = values as string[];
  if (!parts.every((value) => /^\d+(?:\.\d+)?$/.test(value))) return "—";
  const scale = Math.max(...parts.map((value) => value.split(".")[1]?.length ?? 0));
  const divisor = BigInt(10) ** BigInt(scale);
  const total = parts.reduce((sum, value) => {
    const [whole, fraction = ""] = value.split(".");
    return sum + BigInt(whole + fraction.padEnd(scale, "0"));
  }, BigInt(0));
  const average = total / BigInt(parts.length);
  const remainder = total % BigInt(parts.length);
  const precision = Math.max(scale, 6);
  const fractional = (remainder * (BigInt(10) ** BigInt(precision)) / BigInt(parts.length))
    .toString()
    .padStart(precision, "0");
  const value = average / divisor;
  const baseFraction = (average % divisor).toString().padStart(scale, "0");
  const extraFraction = scale === precision ? "" : fractional.slice(scale);
  return `${value}.${(baseFraction + extraFraction).replace(/0+$/, "") || "0"}`;
}

function formatCost(usage: JsonObject | null): string {
  const cost = object(usage?.cost);
  const amount = stringAt(cost, "total_cost");
  if (!amount) return "—";
  return `${amount} ${stringAt(cost, "currency") || "USD"}`;
}

const useColor = process.stdout.isTTY;
const style = (code: number, text: string): string => useColor ? `\u001B[${code}m${text}\u001B[0m` : text;
const title = (text: string): string => style(1, style(36, text));
const muted = (text: string): string => style(2, text);
const success = (text: string): string => style(32, text);
const failure = (text: string): string => style(31, text);

function printReport(specification: string, runs: BenchmarkRun[]): void {
  const costs = runs.map((run) => stringAt(object(run.usage?.cost), "total_cost"));
  const currencies = new Set(runs.map((run) => stringAt(object(run.usage?.cost), "currency")).filter(Boolean));
  const currency = currencies.size === 1 ? ` ${[...currencies][0]}` : "";
  const averageCost = `${averageDecimal(costs)}${currency}`;
  const costColumnWidth = Math.max("Cost".length, averageCost.length, ...runs.map((run) => formatCost(run.usage).length));

  console.log(`\n${title("Benchmark")} ${specification}`);
  console.log(muted([
    "Run ".padEnd(4), "Time (s)".padEnd(9), "Input ".padEnd(6), "Output ".padEnd(7),
    "Cache read".padEnd(11), "Cache write".padEnd(12), "Cost".padEnd(costColumnWidth),
    "Session ID".padEnd(37), "Status",
  ].join(" ")));
  for (const [index, run] of runs.entries()) {
    console.log([
      String(index + 1).padEnd(4),
      (run.elapsedMs === null ? "—" : (run.elapsedMs / 1000).toFixed(2)).padEnd(9),
      averageInteger([numberAt(run.usage, "input_tokens")]).padEnd(6),
      averageInteger([numberAt(run.usage, "output_tokens")]).padEnd(7),
      averageInteger([numberAt(run.usage, "cache_read_input_tokens")]).padEnd(11),
      averageInteger([numberAt(run.usage, "cache_write_input_tokens")]).padEnd(12),
      formatCost(run.usage).padEnd(costColumnWidth),
      (run.sessionId || "—").padEnd(37),
      run.exitCode === 0 ? success("ok") : failure(`failed (${run.exitCode})`),
    ].join(" "));
  }
  console.log([
    title("Avg ").padEnd(4),
    averageNumber(runs.map((run) => run.elapsedMs === null ? null : run.elapsedMs / 1000)).padEnd(9),
    averageInteger(runs.map((run) => numberAt(run.usage, "input_tokens"))).padEnd(6),
    averageInteger(runs.map((run) => numberAt(run.usage, "output_tokens"))).padEnd(7),
    averageInteger(runs.map((run) => numberAt(run.usage, "cache_read_input_tokens"))).padEnd(11),
    averageInteger(runs.map((run) => numberAt(run.usage, "cache_write_input_tokens"))).padEnd(12),
    averageCost.padEnd(costColumnWidth),
    "—".padEnd(37),
  ].join(" "));
}

function printFailures(reports: Array<{ specification: string; runs: BenchmarkRun[] }>): void {
  const failures = reports.flatMap(({ specification, runs }) => runs.flatMap((run, index) =>
    run.exitCode === 0 ? [] : [{ specification, run: index + 1, exitCode: run.exitCode, output: run.output }],
  ));
  if (failures.length === 0) return;

  console.log(`\n${failure("Failed run output")}`);
  for (const item of failures) {
    console.log(`${failure(`Benchmark run ${item.run} (exit ${item.exitCode})`)}: ${item.specification}`);
    console.log(item.output || "(no output captured)");
  }
}

async function main(): Promise<void> {
  const { runs, specifications } = parseBenchmarkOptions(process.argv.slice(2));
  if (specifications.length === 0) {
    console.error("Usage: ./scripts/benchmark-cost.ts [-n RUNS|--runs RUNS] '<cagent exec options and prompt>' [...]");
    process.exitCode = 2;
    return;
  }

  const reports = specifications.map((specification) => ({ specification, command: commandForBenchmark(specification) }));
  console.error(muted("Building cagent…"));
  await buildCagent();
  console.error(muted(`Starting ${runs} parallel run(s) for each of ${reports.length} benchmark(s)…`));
  const completedReports = await Promise.all(reports.map(async ({ specification, command }) => ({
    specification,
    runs: await Promise.all(Array.from({ length: runs }, () => runBenchmark(command))),
  })));
  for (const report of completedReports) printReport(report.specification, report.runs);
  printFailures(completedReports);
  if (completedReports.some((report) => report.runs.some((run) => run.exitCode !== 0))) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    process.exitCode = 1;
  }
}

if (import.meta.main) {
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
