#!/usr/bin/env node
// 一键版本号提升。用法：
//   node tools/bump-version.mjs 1.1.2        （命令行）
//   bump-version.bat                          （双击后输入）
//
// 覆盖 10 处版本位置（与手动 bump 1.0.2→1.1.0 / 1.1.0→1.1.1 的清单一致）：
//   package.json / frontend/package.json / src-tauri/tauri.conf.json（首个 version 字段）
//   src-tauri/Cargo.toml / src-tauri/mcp-launcher/Cargo.toml（首个 version 行）
//   frontend/src/App.tsx（侧栏页脚 v{版本}）
//   package-lock.json / frontend/package-lock.json（根包两个 version 字段，JSON 感知）
//   src-tauri/Cargo.lock / src-tauri/mcp-launcher/Cargo.lock（成员自身条目）
// vault/ 走独立的 0.x 版本线，第三方依赖恰好等于旧版本号的条目，都不动。
//
// 安全策略：先在内存里对全部文件做校验与替换，任何一处对不上就整体不写，
// 不会留下半改状态。
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (relative) => readFileSync(join(root, relative), "utf8");

function fail(message) {
  console.error(`[错误] ${message}`);
  process.exit(1);
}

const next = process.argv[2];
if (!next || !/^\d+\.\d+\.\d+$/.test(next)) {
  fail(`版本号格式应为 X.Y.Z（如 1.1.2），收到：${next ?? "(空)"}`);
}

// 当前版本以 tauri.conf.json 为真相源（安装包与更新器认的就是它）。
const current = read("src-tauri/tauri.conf.json").match(
  /"version": "(\d+\.\d+\.\d+)"/,
)?.[1];
if (!current) fail("无法从 src-tauri/tauri.conf.json 读出当前版本号");
if (current === next) {
  console.log(`版本号已是 ${current}，无需修改。`);
  process.exit(0);
}

/** 把 text 里的第 count 处字面量 from 换成 to；找不到就抛错。 */
function replaceExact(file, text, from, to, count = 1) {
  const hits = text.split(from).length - 1;
  if (hits < count) {
    throw new Error(
      `${file} 里没有找到 ${count} 处 ${JSON.stringify(from)}（实际 ${hits} 处），可能版本状态不一致`,
    );
  }
  return text.replace(from, to);
}

// manifest 的 version 字段都在文件头部，只动第一处，不误伤依赖声明。
const manifestEdits = [
  "package.json",
  "frontend/package.json",
  "src-tauri/tauri.conf.json",
].map((file) => ({
  file,
  apply: (text) =>
    replaceExact(file, text, `"version": "${current}"`, `"version": "${next}"`),
}));

// Cargo.toml 的 [package] 在文件最前，同样只动第一处 version 行。
const cargoManifests = [
  "src-tauri/Cargo.toml",
  "src-tauri/mcp-launcher/Cargo.toml",
].map((file) => ({
  file,
  apply: (text) =>
    replaceExact(
      file,
      text,
      `version = "${current}"`,
      `version = "${next}"`,
    ),
}));

// 侧栏页脚的 v{版本} 展示。
const footerEdit = {
  file: "frontend/src/App.tsx",
  apply: (text) =>
    replaceExact(
      "frontend/src/App.tsx",
      text,
      `v${current}`,
      `v${next}`,
    ),
};

// package-lock：JSON 解析后只改根包两个字段，再按 npm 的格式（2 空格缩进）回写。
function editPackageLock(file, text) {
  const data = JSON.parse(text);
  if (data.version !== current || data.packages?.[""]?.version !== current) {
    throw new Error(`${file} 的根包版本不是 ${current}，可能版本状态不一致`);
  }
  data.version = next;
  data.packages[""].version = next;
  return `${JSON.stringify(data, null, 2)}\n`;
}

// Cargo.lock：按从 Cargo.toml 读到的包名，只改成员自身条目。
// lock 里可能没有成员条目（如某些构建配置），没有就跳过并提示。
function editCargoLock(file, text, packageName) {
  const from = `name = "${packageName}"\nversion = "${current}"`;
  if (!text.includes(from)) {
    console.log(`  跳过 ${file}（没有 ${packageName} 的成员条目）`);
    return text;
  }
  return text.replace(from, `name = "${packageName}"\nversion = "${next}"`);
}

const plans = [
  ...manifestEdits,
  ...cargoManifests,
  footerEdit,
  {
    file: "package-lock.json",
    apply: (text) => editPackageLock("package-lock.json", text),
  },
  {
    file: "frontend/package-lock.json",
    apply: (text) => editPackageLock("frontend/package-lock.json", text),
  },
];

// 第一阶段：全部在内存里改完（任何一处抛错都不会进入写入）。
const outputs = [];
for (const plan of plans) {
  outputs.push({ file: plan.file, text: plan.apply(read(plan.file)) });
}
for (const [lockFile, manifest] of [
  ["src-tauri/Cargo.lock", "src-tauri/Cargo.toml"],
  ["src-tauri/mcp-launcher/Cargo.lock", "src-tauri/mcp-launcher/Cargo.toml"],
]) {
  const name = read(manifest).match(/^name = "(.+)"$/m)?.[1];
  if (!name) fail(`无法从 ${manifest} 读出包名`);
  const text = read(lockFile);
  outputs.push({
    file: lockFile,
    text: editCargoLock(lockFile, text, name),
  });
}

// 第二阶段：统一写回（UTF-8、LF）。
for (const { file, text } of outputs) {
  if (text.includes("\r\n")) fail(`${file} 将被写入 CRLF，已中止`);
  writeFileSync(join(root, file), text, "utf8");
  console.log(`  已更新 ${file}`);
}
console.log(`版本号：${current} → ${next}（共 ${outputs.length} 个文件）`);
