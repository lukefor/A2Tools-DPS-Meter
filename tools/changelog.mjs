// Make the release notes the meter shows in Settings → What's new (#127).
//
//   node tools/changelog.mjs [out file] [count]
//
// Reads the last <count> (default 5) GitHub releases with `gh`, splits each
// body into its languages (the <details><summary>한국어 (Korean)</summary>
// sections every release has), and writes <out file> (default
// changelog-v2.json):
//   { "releases": [ { "version": "2.0.83", "date": "2026-10-28",
//                     "notes": { "en": "**Fixes**\n- ...", "ko": "..." } } ] }
// Newest first. Sign it with tools/sign-manifest.mjs (the update key) and
// upload both: release-template/changelog.sh does all three.
import { execFileSync } from "node:child_process";
import fs from "node:fs";

const [out = "changelog-v2.json", countArg = "5"] = process.argv.slice(2);
const count = Math.max(1, Math.min(50, Number(countArg) || 5));
const REPO = "taengu/A2Tools-DPS-Meter";

const gh = (args) => execFileSync("gh", [...args, "-R", REPO], { encoding: "utf8", timeout: 30_000 });

// A section's <summary> to the meter's language code.
const LANGUAGES = [
  [/english/i, "en"],
  [/korean|한국어/i, "ko"],
  [/simplified|简体/i, "zh-Hans"],
  [/traditional|繁體/i, "zh-Hant"],
  [/japanese|日本語/i, "ja"],
  [/german|deutsch/i, "de"],
  [/french|français/i, "fr"],
  [/spanish|español/i, "es"],
  [/portuguese|português/i, "pt"],
  [/russian|русский/i, "ru"],
];

function split(body) {
  const notes = {};
  const sections = body.matchAll(/<details[^>]*>\s*<summary>([^<]*)<\/summary>([\s\S]*?)<\/details>/g);
  for (const [, summary, text] of sections) {
    const code = LANGUAGES.find(([pattern]) => pattern.test(summary))?.[1];
    if (code && text.trim()) notes[code] = text.trim();
  }
  if (!Object.keys(notes).length) {
    // An old release in one language: its body less the title and footer.
    const text = body
      .split(/\n---\s*\n/)[0]
      .replace(/^#[^\n]*\n/, "")
      .trim();
    if (text) notes.en = text;
  }
  return notes;
}

const list = JSON.parse(gh(["release", "list", "--limit", String(count + 5), "--json", "tagName,isDraft,isPrerelease"]));
const tags = list.filter((r) => !r.isDraft && !r.isPrerelease).slice(0, count).map((r) => r.tagName);
const releases = [];
for (const tag of tags) {
  const { body, publishedAt } = JSON.parse(gh(["release", "view", tag, "--json", "body,publishedAt"]));
  const notes = split(body || "");
  if (!notes.en) {
    console.warn(`${tag}: no English notes, left out`);
    continue;
  }
  releases.push({ version: tag.replace(/^v/i, ""), date: (publishedAt || "").slice(0, 10), notes });
  console.log(`${tag}: ${Object.keys(notes).join(", ")}`);
}
fs.writeFileSync(out, JSON.stringify({ releases }, null, 1) + "\n");
console.log(`wrote ${out}: ${releases.length} releases, ${fs.statSync(out).size} bytes`);
