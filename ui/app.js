const tauri = window.__TAURI__;
const invoke = tauri ? tauri.core.invoke : async (cmd, args) => console.log("invoke", cmd, args);
const $ = (id) => document.getElementById(id);

const CONN_TEXT = {
  searching: "Looking for WalkingPad…",
  connecting: "Connecting…",
  connected: "Connected",
  "no-bluetooth": "Bluetooth unavailable",
};
const MODE_TEXT = { manual: "Manual", auto: "Auto", standby: "Standby" };
const nf = new Intl.NumberFormat();
let state = null;

function send(cmd) {
  invoke("send", { cmd }).catch((e) => console.error(e));
}

function beltKind(belt) {
  if (!belt) return "none";
  if (typeof belt === "object" && "countdown" in belt) return "countdown";
  return typeof belt === "string" ? belt : "other";
}

function render(s) {
  state = s;
  const panel = $("panel");
  panel.dataset.conn = s.connection;
  const kind = beltKind(s.belt);
  panel.dataset.belt = kind;

  $("conn").textContent = s.connection === "connected" && s.device ? s.device : CONN_TEXT[s.connection] ?? s.connection;
  $("mode-label").textContent = s.connection === "connected" && s.mode ? MODE_TEXT[s.mode] : "";

  $("speed").textContent = s.speed.toFixed(1);
  const t = s.target;
  $("target").textContent = t != null && Math.abs(t - s.speed) >= 0.05
    ? (kind === "running" ? `→ ${t.toFixed(1)} km/h` : `Start at ${t.toFixed(1)} km/h`)
    : " ";
  $("count").textContent = kind === "countdown" ? s.belt.countdown : "";

  $("today-steps").textContent = nf.format(s.todaySteps);
  $("today-km").textContent = (s.todayDistanceM / 1000).toFixed(2);
  $("today-min").textContent = Math.round(s.todayActiveS / 60);

  $("session").textContent = s.connection === "connected" && (s.sessionSteps || s.sessionTimeS)
    ? `Session · ${fmtDuration(s.sessionTimeS)} · ${nf.format(s.sessionSteps)} steps`
    : " ";

  $("keep").checked = s.keepRunning;
  renderWeek(s.week);
}

function fmtDuration(sec) {
  const m = Math.floor(sec / 60);
  if (m < 1) return `${sec} s`;
  return m >= 60 ? `${Math.floor(m / 60)} h ${m % 60} min` : `${m} min`;
}

function renderWeek(week) {
  const max = Math.max(1000, ...week.map((d) => d.steps));
  const bars = $("bars");
  bars.replaceChildren(
    ...week.map((d, i) => {
      const el = document.createElement("div");
      el.className = "bar" + (i === week.length - 1 ? " today" : "");
      const date = new Date(d.date + "T12:00:00");
      el.title = `${date.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "short" })}: ${nf.format(d.steps)} steps`;
      const bar = document.createElement("i");
      bar.style.height = `${Math.max(2, (d.steps / max) * 48)}px`;
      const lbl = document.createElement("span");
      lbl.textContent = date.toLocaleDateString(undefined, { weekday: "narrow" });
      el.append(bar, lbl);
      return el;
    }),
  );
}

// Remote keys
$("up").addEventListener("click", (e) => send({ type: "adjust", tenths: e.shiftKey ? 1 : 5 }));
$("down").addEventListener("click", (e) => send({ type: "adjust", tenths: e.shiftKey ? -1 : -5 }));
$("startstop").addEventListener("click", () => send({ type: "startStop" }));

// Mode: click toggles manual <-> auto; press and hold for standby (like the physical remote).
let holdTimer = null, held = false;
const modeBtn = $("mode");
modeBtn.addEventListener("pointerdown", () => {
  held = false;
  holdTimer = setTimeout(() => { held = true; send({ type: "setMode", mode: "standby" }); }, 700);
});
modeBtn.addEventListener("pointerup", () => {
  clearTimeout(holdTimer);
  if (held || !state) return;
  send({ type: "setMode", mode: state.mode === "manual" ? "auto" : "manual" });
});
modeBtn.addEventListener("pointerleave", () => clearTimeout(holdTimer));

$("keep").addEventListener("change", (e) => send({ type: "keepRunning", enabled: e.target.checked }));
$("quit").addEventListener("click", () => invoke("quit"));

// Keyboard: arrows = 0.1 km/h (shift = 0.5), space = start/stop
const flash = (id) => { const k = $(id); k.classList.add("pressed"); setTimeout(() => k.classList.remove("pressed"), 120); };
document.addEventListener("keydown", (e) => {
  if (e.key === "ArrowUp") { send({ type: "adjust", tenths: e.shiftKey ? 5 : 1 }); flash("up"); e.preventDefault(); }
  else if (e.key === "ArrowDown") { send({ type: "adjust", tenths: e.shiftKey ? -5 : -1 }); flash("down"); e.preventDefault(); }
  else if (e.key === " ") { send({ type: "startStop" }); flash("startstop"); e.preventDefault(); }
});

if (tauri) {
  tauri.event.listen("pad", (e) => render(e.payload));
  invoke("get_state").then(render);
} else {
  // Plain-browser preview with sample data, sized like the popover window
  document.body.style.width = "340px";
  document.body.style.height = "510px";
  render({
    connection: "connected", device: "WalkingPad", belt: "running", mode: "manual", speed: 3.2, target: 3.5,
    maxSpeed: 6, sessionTimeS: 1265, sessionDistanceM: 1120, sessionSteps: 1634,
    todaySteps: 6421, todayDistanceM: 4380, todayActiveS: 3900, keepRunning: false,
    week: ["2026-11-02","2026-11-03","2026-11-04","2026-11-05","2026-11-06","2026-11-07","2026-11-08"]
      .map((date, i) => ({ date, steps: [4200, 8100, 0, 6900, 10200, 3100, 6421][i] })),
  });
}
