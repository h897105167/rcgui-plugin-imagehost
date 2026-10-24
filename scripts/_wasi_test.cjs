// 离线验证 imagehost 插件的协议往返：把 JSON 喂给 main.wasm，看它回什么。
// 用法：node scripts/_wasi_test.cjs
const { spawnSync } = require("node:child_process");
const path = require("node:path");

const RUN = path.join(__dirname, "_wasi_run.cjs");

function call(payload) {
  const r = spawnSync(process.execPath, [RUN], {
    input: JSON.stringify(payload),
    encoding: "utf8",
    env: { ...process.env, NODE_NO_WARNINGS: "1" },
  });
  if (r.status !== 0) return { error: `exit ${r.status}: ${(r.stderr || "").slice(-400)}` };
  const out = (r.stdout || "").trim();
  try {
    return JSON.parse(out);
  } catch {
    return { error: `not json: ${out.slice(0, 300)}` };
  }
}

function brief(v) {
  const t = JSON.stringify(v);
  return t.length > 700 ? t.slice(0, 700) + " ..." : t;
}

const now = "2026-10-24T22:00:00+08:00";
const live = { busy: false, label: "", done: 0, total: 0 };
const legacyBed = {
  dedupe: false,
  last_link: "https://kvault.example.com/file/tgs_x.png",
  last_short: "http://i71i.com/letx",
  path: "image_bed",
  path_history: ["image_bed"],
  remote: "k-vault",
  short_callback: "callback",
  short_fmt: 0,
  short_key: "KEY",
  short_method: 0,
  short_rate: 4,
  short_tpl: 1,
  short_url: "http://api.ft12.com/api.php",
  upload_path: "",
};

const steps = [];
steps.push(["manifest", call({ id: 1, method: "manifest", params: {} })]);
steps.push([
  "first-round state (no provided)",
  call({ id: 2, method: "request", params: { action: "state", form: {}, live, now, req_id: "r2" } }),
]);
steps.push([
  "state <- kv(bed) only",
  call({
    id: 3,
    method: "request",
    params: {
      action: "state",
      form: {},
      live,
      now,
      req_id: "r3",
      provided: { kv: { bed: legacyBed } },
      state: { action: "state", step: 0, form: {} },
    },
  }),
]);
steps.push([
  "state <- kv + remotes",
  call({
    id: 4,
    method: "request",
    params: {
      action: "state",
      form: {},
      live,
      now,
      req_id: "r4",
      provided: { kv: { bed: legacyBed }, remotes: ["k-vault", "alist"] },
      state: { action: "state", step: 1, form: {}, cfg: legacyBed },
    },
  }),
]);
steps.push([
  "browse <- dirs",
  call({
    id: 5,
    method: "request",
    params: {
      action: "browse",
      form: { remote: "k-vault", path: "image_bed" },
      live,
      now,
      req_id: "r5",
      provided: {
        kv: { bed: legacyBed },
        list: { list: [{ Name: "2024", IsDir: true }, { Name: "a.png", IsDir: false }] },
      },
      state: { action: "browse", step: 1, form: {}, cfg: legacyBed },
    },
  }),
]);
steps.push([
  "run step0 (inputs ok)",
  call({
    id: 6,
    method: "request",
    params: {
      action: "run",
      form: { remote: "k-vault", path: "image_bed", upload_path: "C:\\tmp\\a.png", short_tpl: "模板一" },
      live,
      now,
      req_id: "r6",
      provided: { kv: { bed: legacyBed } },
      state: { action: "run", step: 0, form: {} },
    },
  }),
]);
steps.push([
  "run step1 (link from site)",
  call({
    id: 7,
    method: "request",
    params: {
      action: "run",
      form: {},
      live,
      now,
      req_id: "r7",
      provided: {
        kv: { bed: legacyBed },
        file: { data_b64: "AAAA", size: 4, name: "a.png" },
        list: { list: [{ Name: "b.png", IsDir: false }] },
        env: { data_dir: "C:\\data", os: "windows" },
      },
      state: {
        action: "run",
        step: 1,
        form: {},
        cfg: { ...legacyBed, upload_path: "C:\\tmp\\a.png" },
        work: { att: [], file: "a.png", env: "" },
      },
    },
  }),
]);
steps.push(["exit", call({ id: 8, method: "request", params: { action: "exit", form: {}, live, now, req_id: "r8" } })]);

for (const [name, out] of steps) {
  console.log(`\n### ${name}\n${brief(out)}`);
}
