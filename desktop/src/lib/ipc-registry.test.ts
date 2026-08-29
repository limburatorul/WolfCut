/**
 * The IPC boundary's two halves against each other.
 *
 * The boundary is JSON, so nothing at compile time checks that a wrapper in
 * `lib/engine.ts` names a command the host actually registered, or that every
 * registered command has a wrapper. This test closes the gap the desktop
 * README used to flag ("nothing checks steps 1 and 3 against each other"),
 * and enforces the standing rule that `lib/engine.ts` is the only file that
 * calls `invoke` at all.
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vitest";

const root = process.cwd();

function read(path: string): string {
  return readFileSync(join(root, path), "utf8");
}

/** Command names the host registers, from the generate_handler! block. */
function registeredCommands(): Set<string> {
  const source = read("src-tauri/src/lib.rs");
  const block = source.match(/generate_handler!\[([\s\S]*?)\]/);
  expect(block, "generate_handler! block in src-tauri/src/lib.rs").toBeTruthy();
  const names = block![1]
    .split(",")
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0)
    // `editor_api::editor_open` registers as `editor_open`.
    .map((entry) => entry.split("::").pop()!);
  return new Set(names);
}

/** Command names the frontend invokes, from lib/engine.ts. */
function invokedCommands(): Set<string> {
  const source = read("src/lib/engine.ts");
  const names = new Set<string>();
  for (const match of source.matchAll(/invoke(?:<[^>]*>)?\(\s*"([a-z_]+)"/g)) {
    names.add(match[1]);
  }
  return names;
}

function walk(directory: string, out: string[] = []): string[] {
  for (const entry of readdirSync(join(root, directory))) {
    const path = join(directory, entry);
    if (statSync(join(root, path)).isDirectory()) walk(path, out);
    else if (/\.(ts|tsx)$/.test(entry) && !/\.test\.tsx?$/.test(entry)) out.push(path);
  }
  return out;
}

describe("the IPC registry", () => {
  test("every wrapper names a registered command", () => {
    const registered = registeredCommands();
    const unregistered = [...invokedCommands()].filter((name) => !registered.has(name));
    expect(unregistered, "wrappers invoking commands the host never registered").toEqual([]);
  });

  test("every registered command has a wrapper", () => {
    const invoked = invokedCommands();
    const orphaned = [...registeredCommands()].filter((name) => !invoked.has(name));
    expect(orphaned, "registered commands nothing in the UI can call").toEqual([]);
  });

  test("lib/engine.ts is the only file that calls invoke", () => {
    const offenders = walk("src").filter(
      (path) =>
        path !== join("src", "lib", "engine.ts") &&
        /\binvoke\s*[(<]/.test(read(path)),
    );
    expect(offenders, "files calling invoke outside the typed boundary").toEqual([]);
  });
});

/**
 * Command *arguments*, as opposed to command names.
 *
 * Tauri matches these by name at runtime, converting camelCase from JS to
 * snake_case in Rust. Nothing at compile time relates the two: a renamed Rust
 * parameter, or a typo in the object literal, produces a command that resolves
 * and then fails - or worse, one that receives `undefined` where a number was
 * meant. This is the same gap the name check above closes, one level down.
 */

/** `thresholdDb` -> `threshold_db`, the conversion Tauri performs. */
function snakeCase(name: string): string {
  return name.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
}

/**
 * Splits a comma-separated list, ignoring commas nested inside brackets.
 *
 * `tauri::State<'_, EditorState>` is one parameter, not two.
 */
function topLevelSplit(source: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let current = "";
  for (const character of source) {
    if ("<([".includes(character)) depth += 1;
    else if (">)]".includes(character)) depth -= 1;
    if (character === "," && depth === 0) {
      parts.push(current);
      current = "";
      continue;
    }
    current += character;
  }
  parts.push(current);
  return parts.map((part) => part.trim()).filter((part) => part.length > 0);
}

/** The text between `open` and its matching close, exclusive. */
function balanced(source: string, open: number, opener: string, closer: string): string {
  let depth = 0;
  for (let index = open; index < source.length; index += 1) {
    if (source[index] === opener) depth += 1;
    else if (source[index] === closer) {
      depth -= 1;
      if (depth === 0) return source.slice(open + 1, index);
    }
  }
  throw new Error("unbalanced " + opener + " at " + open);
}

/** Parameter names of every #[tauri::command], minus the injected ones. */
function commandParameters(): Map<string, string[]> {
  const found = new Map<string, string[]>();
  for (const name of readdirSync(join(root, "src-tauri/src"))) {
    if (!name.endsWith(".rs")) continue;
    const source = read(join("src-tauri/src", name));
    for (const match of source.matchAll(/#\[tauri::command\][^;{]*?\bfn\s+([a-z_0-9]+)\s*\(/g)) {
      const open = match.index! + match[0].length - 1;
      const parameters = topLevelSplit(balanced(source, open, "(", ")"))
        .map((parameter) => {
          const colon = parameter.indexOf(":");
          return {
            name: parameter.slice(0, colon).trim(),
            type: parameter.slice(colon + 1).trim(),
          };
        })
        // app: tauri::AppHandle and state: tauri::State<..> are handed to the
        // command by Tauri, never sent from the window.
        .filter((parameter) => !parameter.type.startsWith("tauri::"))
        .map((parameter) => parameter.name);
      found.set(match[1], parameters);
    }
  }
  return found;
}

/**
 * What each wrapper actually sends, by command name.
 *
 * Only call sites passing an inline object literal can be read statically; the
 * handful that forward a prepared variable (`request`, `session`) are reported
 * as skipped rather than silently passing.
 */
function invokedArguments(): { sent: Map<string, string[]>; skipped: string[] } {
  const source = read("src/lib/engine.ts");
  const sent = new Map<string, string[]>();
  const skipped: string[] = [];
  for (const match of source.matchAll(/invoke(?:<[^>]*>)?\(\s*"([a-z_]+)"\s*(,?)/g)) {
    const command = match[1];
    if (match[2] !== ",") {
      sent.set(command, []);
      continue;
    }
    const after = match.index! + match[0].length;
    const rest = source.slice(after);
    const brace = rest.search(/\S/);
    if (rest[brace] !== "{") {
      skipped.push(command);
      continue;
    }
    const keys = topLevelSplit(balanced(rest, brace, "{", "}")).map((entry) => {
      const colon = entry.indexOf(":");
      return (colon === -1 ? entry : entry.slice(0, colon)).trim();
    });
    sent.set(command, keys);
  }
  return { sent, skipped };
}

describe("the IPC argument names", () => {
  test("every wrapper sends exactly the arguments its command declares", () => {
    const declared = commandParameters();
    const { sent } = invokedArguments();

    const mismatched: string[] = [];
    for (const [command, keys] of sent) {
      const parameters = declared.get(command);
      // A command with no wrapper is the other test's business.
      if (!parameters) continue;
      const wanted = [...parameters].sort();
      const given = keys.map(snakeCase).sort();
      if (wanted.join(",") !== given.join(",")) {
        mismatched.push(`${command}: rust wants [${wanted}], the window sends [${given}]`);
      }
    }
    expect(mismatched, "wrappers whose arguments do not match the command").toEqual([]);
  });

  test("the parser found the commands it is supposed to check", () => {
    // A regex that quietly stopped matching would make the test above pass by
    // checking nothing at all.
    const declared = commandParameters();
    expect(declared.size).toBeGreaterThan(30);
    expect(declared.get("detect_silence")).toEqual(["path", "threshold_db", "min_duration"]);
    expect(declared.get("editor_close")).toEqual([]);
  });
});
