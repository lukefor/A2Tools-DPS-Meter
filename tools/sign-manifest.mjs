// Sign the update manifest for the meter (2.0.80 on).
//
//   node tools/sign-manifest.mjs <latest-v2.json> [key file]
//
// Writes <latest-v2.json>.sig: the Ed25519 signature of the file's exact
// bytes, in hex, which the meter checks against the public key it carries
// (src-tauri/src/update_check.rs) before offering an update. The key file
// (default ~/.a2tools-update-key) holds the private key's 32-byte seed in
// hex; it never leaves this PC and is not the plugin catalog's key.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [manifestPath, keyPath = path.join(os.homedir(), ".a2tools-update-key")] = process.argv.slice(2);
if (!manifestPath) {
  console.error("usage: node tools/sign-manifest.mjs <latest-v2.json> [key file]");
  process.exit(2);
}
const seed = Buffer.from(fs.readFileSync(keyPath, "utf8").trim(), "hex");
if (seed.length !== 32) {
  console.error(`${keyPath}: not a 32-byte Ed25519 seed in hex`);
  process.exit(1);
}
// PKCS#8 wrapping of a raw Ed25519 seed.
const key = crypto.createPrivateKey({
  key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]),
  format: "der",
  type: "pkcs8",
});
const body = fs.readFileSync(manifestPath);
const signature = crypto.sign(null, body, key).toString("hex");
// Checked against the public half before writing, so a wrong key fails here.
const ok = crypto.verify(null, body, crypto.createPublicKey(key), Buffer.from(signature, "hex"));
if (!ok) {
  console.error("signature did not verify");
  process.exit(1);
}
fs.writeFileSync(`${manifestPath}.sig`, signature + "\n");
const pub = crypto.createPublicKey(key).export({ format: "der", type: "spki" }).subarray(-32).toString("hex");
console.log(`signed ${path.basename(manifestPath)} (key ${pub.slice(0, 16)}…)`);
